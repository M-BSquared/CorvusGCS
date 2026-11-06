//! The module that is always there: the routes the foundation owns.

use crate::module::{Module, Registry};

pub(crate) struct Foundation;

impl Module for Foundation {
    fn name(&self) -> &'static str {
        "foundation"
    }

    fn register(&self, _reg: &mut Registry) {}
}
