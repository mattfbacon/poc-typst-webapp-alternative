# Proof-of-Concept Typst Webapp Alternative

With the following features:

- Collaborative editing (using [`operational-transform`](https://docs.rs/operational-transform) and referenced from [ekzhang/rustpad](https://github.com/ekzhang/rustpad))
- Live preview of the code in the browser

It's pretty janky and cursed, but it might be a good starting point if you want to implement an alternative to [typst.app](https://typst.app).

Some notable features that are missing:

- A better renderer for the preview that doesn't flicker and maintains its position when the document changes. Currently we are just embedding a PDF.
- A better editor to complement this, with things like correct syntax highlighting, clicking to go to definitions, inline errors, etc. We may want to switch to Monaco; I used Ace for this because its API is a bit simpler in some ways.
- Packages and plugins. Packages should be fairly simply, plugins not so much.
- Users, projects, files, etc. This is kind of boring and would be pretty easy to bolt on.

PRs welcome if you want to help build this!

## Running

The recommended mode of development is to run the following command from the root of the project:

```shell
pushd wasm; wasm-pack build --dev --target web; popd; cargo run -p server
```

## License

AGPL-3.0-or-later
