// @ts-check

import init, { decode, encode_edit, OperationSeq, Renderer } from "/pkg/typst_webapp_wasm.js";

const debounce = (f, interval) => {
	let timeout = null;
	return (...args) => {
		if (timeout !== null) {
			clearTimeout(timeout);
		}
		timeout = setTimeout(f, interval, ...args);
	};
};

const codepoint_index_to_utf16_index = (text, index_codepoints) => {
	let index_utf16 = 0;

	for (const character of text) {
		if (index_codepoints <= 0) {
			break;
		}

		index_utf16 += character.length;
		index_codepoints -= 1;
	}

	return index_utf16;
};

const utf16_index_to_codepoint_index = (text, index_utf16) => {
	let index_codepoints = 0;

	for (const character of text) {
		if (index_utf16 <= 0) {
			break;
		}

		index_utf16 -= character.length;
		index_codepoints += 1;
	}

	return index_codepoints;
};

const codepoint_index_to_position = (doc, codepoint_index) => {
	const text = doc.getValue();
	const utf16_index = codepoint_index_to_utf16_index(text, codepoint_index);
	return doc.indexToPosition(utf16_index);
};

const position_to_codepoint_index = (doc, position) => {
	const utf16_index = doc.positionToIndex(position);
	const value = doc.getValue();
	const codepoint_index = utf16_index_to_codepoint_index(value, utf16_index);
	return codepoint_index;
};

const count_codepoints = (text) => {
	let count = 0;

	for (const _ of text) {
		count += 1;
	}

	return count;
};

const editor = ace.edit('editor');

await init();

let ignore_changes = 0;
let last_seen_revision = 0;
// If not `null`, this operation has been sent to the server but the server has not yet acknowledged it.
let outstanding_operation = null;
// If not `null`, this operation is waiting to be sent to the server because there was already an outstanding operation when this operation happened.
let next_operation = null;

const apply_operation = (operation) => {
	if (operation.is_noop()) {
		return;
	}

	const edits = operation.to_js();

	let index_codepoints = 0;
	const doc = editor.session.getDocument();

	for (const edit of edits) {
		switch (edit.kind) {
			case 'insert':
				const position = codepoint_index_to_position(doc, index_codepoints);
				ignore_changes += 1;
				doc.insert(position, edit.text);
				index_codepoints += edit.len_codepoints;
				break;
			case 'retain':
				index_codepoints += edit.num_codepoints;
				break;
			case 'delete':
				const text = doc.getValue();
				const from_index = codepoint_index_to_utf16_index(text, index_codepoints);
				const len_utf16 = codepoint_index_to_utf16_index(text, edit.num_codepoints);
				const to_index = from_index + len_utf16;

				const from = doc.indexToPosition(from_index);
				const to = doc.indexToPosition(to_index);

				ignore_changes += 1;
				doc.remove({ start: from, end: to });
				break;
		}
	}
};

const apply_server = (operation) => {
	if (outstanding_operation !== null) {
		[outstanding_operation, operation] = outstanding_operation.transform(operation);
		if (next_operation !== null) {
			[next_operation, operation] = next_operation.transform(operation);
		}
	}

	apply_operation(operation);
};

const handle_history = (start, operations) => {
	if (start > last_seen_revision) {
		console.error("history start is past the last revision we've seen");
		socket.close();
		alert("Your editor has become out of sync with the server. Please save any work, then reload the tab.");
		return;
	}

	for (const operation of operations.slice(last_seen_revision - start)) {
		apply_server(OperationSeq.decode(operation));
		last_seen_revision += 1;
	}
};

const send_operation = (operation) => {
	socket.send(encode_edit(last_seen_revision, operation));
};

const server_ack = (up_to) => {
	last_seen_revision = up_to;

	if (outstanding_operation === null) {
		console.warn("received server ack without an outstanding operation");
		return;
	}
	outstanding_operation = next_operation;
	next_operation = null;
	if (outstanding_operation) {
		send_operation(outstanding_operation);
	}
};

const apply_client = (operation) => {
	console.log("apply_client");
	if (outstanding_operation === null) {
		send_operation(operation);
		outstanding_operation = operation;
	} else if (next_operation === null) {
		next_operation = operation;
	} else {
		next_operation = next_operation.compose(operation);
	}
};

const on_insert = (text, start_pos, end_pos) => {
	console.log("on_insert");
	const doc = editor.session.getDocument();
	const start_index_codepoints = position_to_codepoint_index(doc, start_pos);
	const end_index_codepoints = position_to_codepoint_index(doc, end_pos);
	const len_codepoints = count_codepoints(doc.getValue());

	let operation = new OperationSeq();
	operation.retain(start_index_codepoints);
	operation.insert(text);
	operation.retain(len_codepoints - end_index_codepoints);

	apply_client(operation);
};

const on_delete = (text, start_pos, end_pos) => {
	const doc = editor.session.getDocument();
	const start_index_codepoints = position_to_codepoint_index(doc, start_pos);
	const deleted_len_codepoints = count_codepoints(text);
	const len_codepoints = count_codepoints(doc.getValue());

	let operation = new OperationSeq();
	operation.retain(start_index_codepoints);
	operation.delete_(deleted_len_codepoints);
	operation.retain(len_codepoints - start_index_codepoints);

	apply_client(operation);
};

editor.on('change', delta => {
	console.log("on_change", delta);

	if (ignore_changes > 0) {
		console.log("ignored");
		ignore_changes -= 1;
		return;
	}

	switch (delta.action) {
		case 'insert':
			on_insert(delta.lines.join('\n'), delta.start, delta.end);
			break;
		case 'remove':
			on_delete(delta.lines.join('\n'), delta.start, delta.end);
			break;
		default:
			console.warn(`unknown delta action ${delta.action}, ignoring`);
	}
});


const renderer = new Renderer();
let old_url = null;
editor.on('change', debounce(_ => {
	console.log('re-rendering');

	const doc = editor.session.getDocument();

	const res = renderer.render(doc.getValue());
	console.log('render result', res);

	let url = null;

	const errors = res.pdf === undefined;
	document.getElementById('viewer').classList.toggle('error', errors);
	if (!errors) {
		const blob = new Blob([res.pdf.buffer], { type: 'application/pdf' });
		url = URL.createObjectURL(blob);
		console.log('url', url);
		const frame = document.getElementById('viewer-frame');
		frame.src = url;
	}

	if (old_url !== null) {
		URL.revokeObjectURL(old_url);
	}
	old_url = url;

	editor.session.setAnnotations(res.diagnostics.map(diagnostic => {
		const position = doc.indexToPosition(diagnostic.start_index_utf16);
		return {
			text: diagnostic.message + diagnostic.hints.map(hint => '\n\n' + hint).join(''),
			type: diagnostic.severity,
			row: position.row,
			column: position.column,
		};
	}));

	document.getElementById('viewer').classList.toggle('loaded', true);
}, 50));

const socket = new WebSocket(location.origin.replace('http', 'ws') + '/socket');
socket.binaryType = 'arraybuffer';
socket.onerror = _ => alert('websocket error. check the console for more info.');
socket.onmessage = (raw_message) => {
	const message = decode(new Uint8Array(raw_message.data));
	console.log(message);
	switch (message.kind) {
		case 'init':
			ignore_changes += 1;
			editor.session.getDocument().setValue(message.text);
			last_seen_revision = message.revision;
			break;
		case 'history':
			handle_history(message.start, message.operations);
			break;
		case 'ack':
			server_ack(message.up_to);
			break;
		case 'out_of_sync':
			socket.close();
			alert('Sorry, your document became out of sync. Please save any changes and reload the page.');
			break;
		default:
			console.error(`unknown message kind ${message.kind}, ignoring`);
	}
};
