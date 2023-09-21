use std::{ops::Deref, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeIdentifer {
    node_name: Arc<str>,
}

impl NodeIdentifer {
    pub(crate) fn new(node_name: impl Into<Arc<str>>) -> Self {
        Self {
            node_name: node_name.into(),
        }
    }
}

impl Deref for NodeIdentifer {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.node_name
    }
}
