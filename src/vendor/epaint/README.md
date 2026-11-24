# epaint 0.37.0, with one change

The crate as published (MIT or Apache 2.0; benches, lints and dev dependencies left out), used
through `[patch.crates-io]` in the root `Cargo.toml`. The change is in `src/text/face_store.rs` and
marked "Corvus GCS": the key of a font face is made from its name, not from a counter in a `static`.

Why: a plugin library carries its own copy of every `static`. A font that is first needed while a
plugin draws (a callsign in a script our fonts lack) was numbered from the library's counter, got
the number of the program's own first font and replaced it in the whole window. The test
`a_font_found_by_a_library_does_not_replace_the_programs` in `src/plugins/loader.rs` fails without it.

To update with egui: copy the new version here, put the change back, run that test.
