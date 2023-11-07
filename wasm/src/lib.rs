#![deny(
	absolute_paths_not_starting_with_crate,
	keyword_idents,
	macro_use_extern_crate,
	meta_variable_misuse,
	missing_abi,
	non_ascii_idents,
	nonstandard_style,
	noop_method_call,
	pointer_structural_match,
	private_in_public,
	rust_2018_idioms,
	unused_qualifications
)]
#![warn(clippy::pedantic)]
// #![forbid(unsafe_code)]

use std::borrow::Cow;

use comemo::Prehashed;
use js_sys::{Array, Object, Uint8Array};
use protocol::{ClientMessage, Revision, ServerMessage};
use serde::Serialize;
use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::eval::{Bytes, Datetime, Library, Tracer};
use typst::font::{Font, FontBook};
use typst::syntax::{FileId, Source};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};

#[wasm_bindgen]
extern "C" {
	type ObjectExt;

	#[wasm_bindgen(method, indexing_setter, structural)]
	fn set_property(_: &ObjectExt, name: &str, value: JsValue);
}

#[wasm_bindgen(start)]
pub fn start() {
	console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub fn decode(data: &[u8]) -> Result<JsValue, JsValue> {
	protocol::decode::<ServerMessage<'static>>(data)
		.map(|message| {
			serde_wasm_bindgen::to_value(&message)
				.unwrap_or_else(|error| unreachable!("encode ServerMessage to JS value failed: {error}"))
		})
		.map_err(|error| error.to_string().into())
}

#[wasm_bindgen]
pub fn encode_edit(
	last_seen_revision: Revision,
	operations: &OperationSeq,
) -> Result<Uint8Array, JsValue> {
	let message = ClientMessage::Edit {
		last_seen_revision,
		operations: Cow::Borrowed(&operations.0),
	};
	let data = protocol::encode(&message);
	Ok(data.as_slice().into())
}

#[wasm_bindgen]
pub struct OperationSeq(operational_transform::OperationSeq);

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsOperation<'a> {
	Insert { text: &'a str, len_codepoints: u32 },
	Retain { num_codepoints: u32 },
	Delete { num_codepoints: u32 },
}

fn operation_to_js(operation: &operational_transform::Operation) -> JsValue {
	use operational_transform::Operation as O;

	let js = match operation {
		O::Insert(text) => JsOperation::Insert {
			text,
			len_codepoints: text.chars().count().try_into().unwrap(),
		},
		&O::Delete(n) => JsOperation::Delete {
			num_codepoints: n.try_into().unwrap(),
		},
		&O::Retain(n) => JsOperation::Retain {
			num_codepoints: n.try_into().unwrap(),
		},
	};

	serde_wasm_bindgen::to_value(&js).unwrap()
}

#[wasm_bindgen]
impl OperationSeq {
	#[wasm_bindgen(constructor)]
	pub fn new() -> OperationSeq {
		Self(operational_transform::OperationSeq::default())
	}

	#[must_use]
	pub fn decode(raw: JsValue) -> Result<OperationSeq, JsValue> {
		serde_wasm_bindgen::from_value(raw)
			.map(Self)
			.map_err(|error| error.to_string().into())
	}

	#[must_use]
	pub fn is_noop(&self) -> bool {
		self.0.is_noop()
	}

	#[must_use]
	pub fn to_js(&self) -> JsValue {
		self
			.0
			.ops()
			.iter()
			.map(operation_to_js)
			.collect::<Array>()
			.into()
	}

	#[must_use]
	pub fn transform(&self, other: &OperationSeq) -> Result<Box<[OperationSeq]>, JsValue> {
		let (a, b) = self
			.0
			.transform(&other.0)
			.map_err(|error| error.to_string())?;
		Ok([a, b].map(Self).into())
	}

	pub fn compose(&self, other: &OperationSeq) -> Result<OperationSeq, JsValue> {
		self
			.0
			.compose(&other.0)
			.map(Self)
			.map_err(|error| error.to_string().into())
	}

	pub fn retain(&mut self, n: u32) {
		self.0.retain(n.into());
	}

	pub fn delete_(&mut self, n: u32) {
		self.0.delete(n.into());
	}

	pub fn insert(&mut self, text: &str) {
		self.0.insert(text);
	}
}

fn load_fonts() -> Vec<Font> {
	[include_bytes!("../../res/fonts/InriaSerif-Regular.ttf").as_slice()]
		.into_iter()
		.flat_map(|bytes| {
			let buffer = Bytes::from_static(bytes);
			let face_count = ttf_parser::fonts_in_collection(&buffer).unwrap_or(1);
			(0..face_count).map(move |face_idx| Font::new(buffer.clone(), face_idx).unwrap())
		})
		.collect()
}

struct Sandbox {
	library: Prehashed<Library>,
	book: Prehashed<FontBook>,
	fonts: Vec<Font>,
}

impl Sandbox {
	fn new() -> Self {
		let fonts = load_fonts();

		Self {
			library: Prehashed::new(typst_library::build()),
			book: Prehashed::new(FontBook::from_fonts(&fonts)),
			fonts,
		}
	}

	fn with_source(&self, code: String) -> WithSource<'_> {
		let main_source = Source::detached(code);
		let local_time = {
			let js_date = js_sys::Date::new_0();
			let raw_timestamp = js_date.value_of() as i128 * 1_000_000;
			let utc_time = time::OffsetDateTime::from_unix_timestamp_nanos(raw_timestamp).unwrap();
			let raw_offset = (js_date.get_timezone_offset() * 60.0) as i32;
			let local_offset = time::UtcOffset::from_whole_seconds(raw_offset).unwrap();
			utc_time.to_offset(local_offset)
		};

		WithSource {
			sandbox: self,
			main_source,
			local_time,
		}
	}
}

struct WithSource<'a> {
	sandbox: &'a Sandbox,
	main_source: Source,
	local_time: time::OffsetDateTime,
}

impl typst::World for WithSource<'_> {
	fn library(&self) -> &Prehashed<Library> {
		&self.sandbox.library
	}

	fn book(&self) -> &Prehashed<FontBook> {
		&self.sandbox.book
	}

	fn main(&self) -> Source {
		self.main_source.clone()
	}

	fn source(&self, id: FileId) -> FileResult<Source> {
		if id == self.main_source.id() {
			Ok(self.main_source.clone())
		} else {
			Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
		}
	}

	fn file(&self, id: FileId) -> FileResult<Bytes> {
		Err(FileError::NotFound(id.vpath().as_rootless_path().into()))
	}

	fn font(&self, index: usize) -> Option<Font> {
		self.sandbox.fonts.get(index).cloned()
	}

	fn today(&self, offset: Option<i64>) -> Option<Datetime> {
		let time = match offset {
			Some(hours) => {
				let offset = time::UtcOffset::from_hms(hours.try_into().ok()?, 0, 0).ok()?;
				self.local_time.to_offset(offset)
			}
			None => self.local_time,
		};
		Some(Datetime::Date(time.date()))
	}
}

#[wasm_bindgen]
pub struct Renderer {
	sandbox: Sandbox,
}

fn diagnostic_to_js(world: &impl typst::World, diagnostic: &SourceDiagnostic) -> JsValue {
	#[derive(Serialize)]
	struct Helper<'a> {
		severity: &'a str,
		start_index_utf16: u32,
		end_index_utf16: u32,
		message: &'a str,
		#[serde(with = "serde_wasm_bindgen::preserve")]
		hints: JsValue,
	}

	let severity = match diagnostic.severity {
		Severity::Error => "error",
		Severity::Warning => "warning",
	};

	let file = diagnostic
		.span
		.id()
		.map_or_else(|| world.main(), |id| world.source(id).unwrap());
	let range = file.range(diagnostic.span).unwrap();

	let start_index_utf16 = file.byte_to_utf16(range.start).unwrap().try_into().unwrap();
	let end_index_utf16 = file.byte_to_utf16(range.end).unwrap().try_into().unwrap();

	let hints = diagnostic
		.hints
		.iter()
		.map(|hint| JsValue::from_str(hint))
		.collect::<Array>()
		.into();

	let helper = Helper {
		severity,
		start_index_utf16,
		end_index_utf16,
		message: &diagnostic.message,
		hints,
	};
	serde_wasm_bindgen::to_value(&helper).unwrap()
}

#[wasm_bindgen]
impl Renderer {
	#[wasm_bindgen(constructor)]
	#[must_use]
	pub fn new() -> Renderer {
		Renderer {
			sandbox: Sandbox::new(),
		}
	}

	#[must_use]
	pub fn render(&self, code: String) -> JsValue {
		let mut tracer = Tracer::new();
		let world = self.sandbox.with_source(code);

		let res =
			typst::compile(&world, &mut tracer).map(|document| typst::export::pdf(&document, None, None));
		let mut diagnostics = tracer.warnings();

		let object = Object::new().unchecked_into::<ObjectExt>();

		match res {
			Ok(pdf) => {
				let blob = Uint8Array::from(pdf.as_slice());
				object.set_property("pdf", blob.into());
			}
			Err(errors) => {
				diagnostics.extend(errors);
			}
		}

		object.set_property(
			"diagnostics",
			diagnostics
				.iter()
				.map(|diagnostic| diagnostic_to_js(&world, diagnostic))
				.collect::<Array>()
				.into(),
		);

		object.into()
	}
}
