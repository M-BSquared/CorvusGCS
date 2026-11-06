//! The facade of the shared controls (python/src/css/components.css). Each
//! lives in its own file; `kit::` keeps every path a page already uses valid:
//!
//! * `button.rs`: `Button`, `Variant`, `icon_btn` (`.btn`, `.icon-btn`)
//! * `select.rs`: `Choice`, `select`, `select_padded` (`.field-select`)
//! * `toggle.rs`: `checkbox`, `switch` (`.link-toggle`, `.ui-toggle`)
//! * `text.rs`: `wrapped`, `wrapped_weight`, `dot`
//! * `field.rs`: `text_field`, `text_field_padded` (`.field-input`)

pub use crate::ui::button::{icon_btn, Button, Variant};
pub use crate::ui::field::{text_field, text_field_padded};
pub use crate::ui::select::{select, select_padded, Choice};
pub use crate::ui::text::{dot, wrapped, wrapped_weight};
pub use crate::ui::toggle::{checkbox, switch};
