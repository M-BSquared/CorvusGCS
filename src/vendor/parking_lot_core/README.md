# parking_lot_core 0.9.12, with one change

The crate as published (MIT or Apache 2.0, the licences are beside this file), used through
`[patch.crates-io]` in the root `Cargo.toml`. The change, all in `src/parking_lot.rs` and marked
"Corvus GCS": `table_handle` and `adopt_table`, and the table is reached through
`hashtable_cell()`.

Why: egui guards its `Context` with `parking_lot`, whose queue of waiting threads is one `static`
table. A plugin library carries its own copy of every `static`, so a thread that waited through one
copy was never woken by an unlock through the other, and stayed asleep for good. With the change a
library uses the program's table (`corvus_api::plugin::dynamic::apply`). The test
`a_library_and_the_program_wake_each_other` in `src/plugins/loader.rs` fails without it.

To update: copy the new version here, put the change back, run that test.
