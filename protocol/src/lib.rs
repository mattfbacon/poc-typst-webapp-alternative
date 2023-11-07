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

use std::borrow::Cow;

use operational_transform::OperationSeq;
use serde::{Deserialize, Serialize};

pub type Revision = usize;

/// A message from the client to the server.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage<'a> {
	/// "Server, please apply `operations` on top of the document as it was at `last_seen_revision`."
	Edit {
		last_seen_revision: Revision,
		operations: Cow<'a, OperationSeq>,
	},
	/// This is never sent from the client, but it's used to simplify the server logic.
	#[serde(skip)]
	Disconnected,
}

/// A message from the server to the client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage<'a> {
	Init {
		revision: Revision,
		text: Cow<'a, str>,
	},
	/// "Client, since `start`, `operations` have occurred."
	History {
		start: Revision,
		operations: Cow<'a, [OperationSeq]>,
	},
	Ack {
		up_to: Revision,
	},
	OutOfSync,
}

/// Encode a value using the standard wire format.
pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
	let mut buf = Vec::new();
	ciborium::into_writer(v, &mut buf).unwrap_or_else(|error| unreachable!("encode error: {error}"));
	buf
}

/// Decode a value using the standard wire format.
///
/// # Errors
///
/// If the data is invalid.
pub fn decode<T: for<'de> Deserialize<'de>>(
	raw: &[u8],
) -> Result<T, ciborium::de::Error<std::io::Error>> {
	ciborium::from_reader::<T, _>(raw)
}
