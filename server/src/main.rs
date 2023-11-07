#![deny(
	absolute_paths_not_starting_with_crate,
	keyword_idents,
	macro_use_extern_crate,
	meta_variable_misuse,
	missing_abi,
	missing_copy_implementations,
	non_ascii_idents,
	nonstandard_style,
	noop_method_call,
	pointer_structural_match,
	private_in_public,
	rust_2018_idioms,
	unused_qualifications
)]
#![warn(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::extract::ws::WebSocket;
use axum::extract::{self};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use operational_transform::OperationSeq;
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use protocol::{ClientMessage, Revision, ServerMessage};
use tokio::sync::Notify;

struct Client {
	socket: WebSocket,
}

struct PreparedSend<'a> {
	client: &'a mut Client,
	data: Vec<u8>,
}

impl PreparedSend<'_> {
	async fn finish(self) -> Result<()> {
		Ok(self.client.socket.send(self.data.into()).await?)
	}
}

impl Client {
	async fn send(&mut self, message: &ServerMessage<'_>) -> Result<()> {
		self.prepare_send(message).finish().await
	}

	fn prepare_send(&mut self, message: &ServerMessage<'_>) -> PreparedSend<'_> {
		PreparedSend {
			client: self,
			data: protocol::encode(message),
		}
	}

	// TODO avoid possible DOS from excessive pings or pongs.
	async fn recv(&mut self) -> Result<ClientMessage<'static>> {
		use axum::extract::ws::Message as WM;

		loop {
			let message = self.socket.recv().await.transpose()?;
			break match message {
				None | Some(WM::Close(_)) => Ok(ClientMessage::Disconnected),
				Some(WM::Text(_)) => Err(anyhow!("unexpected message")),
				Some(WM::Binary(data)) => {
					let message = protocol::decode(&data).map_err(axum::Error::new)?;
					Ok(message)
				}
				Some(WM::Ping(ping)) => {
					self.socket.send(WM::Pong(ping)).await?;
					continue;
				}
				Some(WM::Pong(_)) => {
					continue;
				}
			};
		}
	}
}

struct State {
	text: String,
	operations: Vec<OperationSeq>,
}

impl State {
	pub fn from_text(text: String) -> Self {
		let mut operation = OperationSeq::default();
		operation.insert(&text);
		Self {
			operations: vec![operation],
			text,
		}
	}
}

struct Editor {
	state: RwLock<State>,
	operations_notify: Notify,
}

impl Editor {
	/// Returns the new latest revision that the client has seen.
	async fn send_history(&self, from: Revision, client: &mut Client) -> Result<()> {
		let message = {
			let state = self.state.read();

			let operations = &state.operations[from..];
			if operations.is_empty() {
				return Ok(());
			}

			client.prepare_send(&ServerMessage::History {
				start: from,
				operations: operations.into(),
			})
		};

		message.finish().await?;

		Ok(())
	}

	fn apply_edit(&self, from: Revision, mut operation: OperationSeq) -> Result<()> {
		tracing::info!(?from, ?operation, "edit");

		// https://en.wikipedia.org/wiki/Readers–writer_lock#Upgradable_RW_lock
		let state = self.state.upgradable_read();
		let history_operations = state.operations.get(from..).ok_or_else(|| {
			anyhow!(
				"client pushed operations after revision {from}, but we only have up to {}",
				state.operations.len(),
			)
		})?;

		for history_operation in history_operations {
			operation = operation
				.transform(history_operation)
				.context("transforming client operation with history operation")?
				.0;
		}

		let new_text = operation
			.apply(&state.text)
			.context("applying transformed operation to server document")?;

		let mut state = RwLockUpgradableReadGuard::upgrade(state);
		state.operations.push(operation);
		state.text = new_text;

		Ok(())
	}
}

enum MessageAction {
	Ok,
	Disconnected,
	OutOfSync,
}

impl Editor {
	fn handle_message(&self, message: ClientMessage<'_>) -> Result<MessageAction> {
		match message {
			ClientMessage::Edit {
				last_seen_revision: revision,
				operations,
			} => {
				if let Err(error) = self.apply_edit(revision, operations.into_owned()) {
					tracing::error!(%error, "client desynchronization");
					return Ok(MessageAction::OutOfSync);
				}
				self.operations_notify.notify_waiters();
			}
			ClientMessage::Disconnected => return Ok(MessageAction::Disconnected),
		}

		Ok(MessageAction::Ok)
	}

	async fn handle_connection(&self, mut client: Client) -> Result<()> {
		// Initialize client.
		let init_message = {
			let state = self.state.read();
			client.prepare_send(&ServerMessage::Init {
				revision: state.operations.len(),
				text: state.text.as_str().into(),
			})
		};
		init_message.finish().await?;

		let mut seen_operations = self.state.read().operations.len();
		let mut must_send = true;

		loop {
			let notified = self.operations_notify.notified();

			let new_revisions = self.state.read().operations.len();
			if new_revisions > seen_operations {
				if must_send {
					self.send_history(seen_operations, &mut client).await?;
				} else {
					let message = ServerMessage::Ack {
						up_to: new_revisions,
					};
					client.send(&message).await?;
				}

				seen_operations = new_revisions;
			}

			must_send = false;
			tokio::select! {
				() = notified => {
					must_send = true;
				}
				message = client.recv() => match self.handle_message(message?)? {
					MessageAction::Ok => {}
					MessageAction::Disconnected => break,
					MessageAction::OutOfSync => {
						client.send(&ServerMessage::OutOfSync).await?;
						break;
					}
				}
			}
		}

		Ok(())
	}
}

async fn socket(
	extract::State(state): extract::State<Arc<Editor>>,
	upgrade: extract::WebSocketUpgrade,
) -> Response {
	tracing::info!("socket connection");
	upgrade.on_upgrade(move |socket| async move {
		if let Err(error) = state.handle_connection(Client { socket }).await {
			tracing::error!(%error, "client error");
		}
	})
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
	tracing_subscriber::fmt::init();

	let editor = Editor {
		state: State::from_text("hello, world!".into()).into(),
		operations_notify: Notify::new(),
	};
	let editor = Arc::new(editor);

	let app = Router::new()
		.route_service(
			"/",
			tower_http::services::fs::ServeFile::new("res/index.html"),
		)
		.route("/socket", get(socket))
		.nest_service("/res", tower_http::services::fs::ServeDir::new("res"))
		.nest_service("/pkg", tower_http::services::fs::ServeDir::new("wasm/pkg"))
		.with_state(editor);

	let address = "127.0.0.1:3000".parse().unwrap();
	tracing::info!(%address, "listening");
	axum::Server::bind(&address)
		.serve(app.into_make_service())
		.await
		.unwrap();
}
