//! The part of the Corvus GCS interface that plugins build on: the widgets
//! and look of the interface (`ui`), the translation layer (`i18n`), the
//! backend client (`net`) and the number and time helpers (`data`). The
//! program re-exports these, so inside it they are still `crate::ui` and so on.

pub mod data;
pub mod i18n;
pub mod net;
pub mod plugin;
pub mod ui;
