use std::time::Duration;

use tokio::{task::JoinHandle, time::Instant};

use crate::rt::Rt;

pub(crate) enum Software {
    Client(ClientSoftware),
    Simulated(Rt),
}

pub(crate) struct ClientSoftware {
    rt: Rt,
    handle: JoinHandle<()>,
}

impl ClientSoftware {
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Software {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<()>) -> Self {
        Self::Client(ClientSoftware { rt, handle })
    }

    pub(crate) fn simulated(rt: Rt) -> Self {
        Self::Simulated(rt)
    }

    pub(crate) fn tick(&self, duration: Duration) -> Instant {
        let rt = match self {
            Software::Client(cs) => &cs.rt,
            Software::Simulated(rt) => rt,
        };

        rt.tick(duration)
    }
}
