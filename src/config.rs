use rand_distr::Exp;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, SystemTime},
};

#[derive(Clone)]
pub(crate) struct Config {
    /// How long the test should run for
    pub(crate) duration: Duration,

    /// How much simulated time should elapse each tick
    pub(crate) tick: Duration,

    /// When the simulation starts
    pub(crate) epoch: SystemTime,
}

/// The kinds of networks that can be simulated in turmoil
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IpNetwork {
    /// An Ipv4 network with an address space of 192.168.0.0/16
    #[default]
    V4,
    /// An local area Ipv6 network with an address space of fe80::/64
    V6,
}

impl IpNetwork {
    pub(crate) fn iter(&self) -> IpNetworkAddrIter {
        match self {
            Self::V4 => IpNetworkAddrIter::V4(1),
            Self::V6 => IpNetworkAddrIter::V6(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IpNetworkAddrIter {
    /// the next ip addr without the network prefix, as u32
    V4(u32),
    /// the next ip addr without the network prefix, as u128
    V6(u128),
}

impl Default for IpNetworkAddrIter {
    fn default() -> Self {
        Self::V4(1)
    }
}

impl IpNetworkAddrIter {
    pub(crate) fn next(&mut self) -> IpAddr {
        match self {
            Self::V4(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = (host >> 8) as u8;
                let b = (host & 0xFF) as u8;

                IpAddr::V4(Ipv4Addr::new(192, 168, a, b))
            }
            Self::V6(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = ((host >> 48) & 0xffff) as u16;
                let b = ((host >> 32) & 0xffff) as u16;
                let c = ((host >> 16) & 0xffff) as u16;
                let d = (host & 0xffff) as u16;

                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, a, b, c, d))
            }
        }
    }
}

/// Configures link behavior.
#[derive(Clone, Default)]
pub(crate) struct Link {
    /// Message latency between two hosts
    pub(crate) latency: Option<Latency>,

    /// How often sending a message works vs. the message getting dropped
    pub(crate) message_loss: Option<MessageLoss>,
}

/// Configure latency behavior between two hosts.
#[derive(Clone)]
pub(crate) struct Latency {
    /// Minimum latency
    pub(crate) min_message_latency: Duration,

    /// Maximum latency
    pub(crate) max_message_latency: Duration,

    /// Probability distribution of latency within the range above.
    pub(crate) latency_distribution: Exp<f64>,
}

/// Configure how often messages are lost
#[derive(Clone)]
pub(crate) struct MessageLoss {
    /// Probability of a link failing
    pub(crate) fail_rate: f64,

    /// Probability of a failed link returning
    pub(crate) repair_rate: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration: Duration::from_secs(10),
            tick: Duration::from_millis(1),
            epoch: SystemTime::now(),
        }
    }
}

impl Link {
    pub(crate) fn latency(&self) -> &Latency {
        self.latency.as_ref().expect("`Latency` missing")
    }

    pub(crate) fn latency_mut(&mut self) -> &mut Latency {
        self.latency.as_mut().expect("`Latency` missing")
    }

    pub(crate) fn message_loss(&self) -> &MessageLoss {
        self.message_loss.as_ref().expect("`MessageLoss` missing")
    }

    pub(crate) fn message_loss_mut(&mut self) -> &mut MessageLoss {
        self.message_loss.as_mut().expect("`MessageLoss` missing")
    }
}

impl Default for Latency {
    fn default() -> Latency {
        Latency {
            min_message_latency: Duration::from_millis(0),
            max_message_latency: Duration::from_millis(100),
            latency_distribution: Exp::new(5.0).unwrap(),
        }
    }
}

impl Default for MessageLoss {
    fn default() -> MessageLoss {
        MessageLoss {
            fail_rate: 0.0,
            repair_rate: 1.0,
        }
    }
}
