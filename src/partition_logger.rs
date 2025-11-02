use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::TRACING_TARGET;

/// Tracks partitioned connections and logs them.
///
/// This utility helps detect and log when connections experience partitioning
/// or slow behavior beyond specified thresholds, which could lead to hangs.
pub struct PartitionLogger {
    /// Tracks IPs with partitioned connections
    partitioned_pairs: HashSet<(IpAddr, IpAddr)>,
    
    /// Tracks partitioned connections with the time they were first detected
    partitions_timestamps: HashMap<(IpAddr, IpAddr), Instant>,
    
    /// Threshold after which to warn about long-lasting partitions
    slow_connection_threshold: Duration,
}

impl PartitionLogger {
    /// Create a new partition logger
    pub fn new(slow_connection_threshold: Duration) -> Self {
        Self {
            partitioned_pairs: HashSet::new(),
            partitions_timestamps: HashMap::new(),
            slow_connection_threshold,
        }
    }
    
    /// Logs information about a partitioned connection between two hosts
    pub fn log_partition(&mut self, src: IpAddr, dst: IpAddr, state: &str) {
        let pair = if src < dst { (src, dst) } else { (dst, src) };
        
        // If this is a new partition, record its timestamp
        if !self.partitioned_pairs.contains(&pair) {
            debug!(
                target: TRACING_TARGET,
                src = ?src,
                dst = ?dst,
                state = ?state,
                "Connection partitioned"
            );
            
            self.partitioned_pairs.insert(pair);
            self.partitions_timestamps.insert(pair, Instant::now());
        }
    }
    
    /// Logs information about a repaired connection
    pub fn log_repair(&mut self, src: IpAddr, dst: IpAddr) {
        let pair = if src < dst { (src, dst) } else { (dst, src) };
        
        if self.partitioned_pairs.remove(&pair) {
            if let Some(start_time) = self.partitions_timestamps.remove(&pair) {
                let duration = start_time.elapsed();
                
                if duration >= self.slow_connection_threshold {
                    warn!(
                        target: TRACING_TARGET,
                        src = ?src,
                        dst = ?dst,
                        duration_ms = ?duration.as_millis(),
                        "Slow connection detected and repaired"
                    );
                } else {
                    debug!(
                        target: TRACING_TARGET,
                        src = ?src,
                        dst = ?dst,
                        duration_ms = ?duration.as_millis(),
                        "Connection repaired"
                    );
                }
            }
        }
    }
    
    /// Check for any long-lasting partitions and log warnings
    pub fn check_slow_connections(&mut self) {
        let now = Instant::now();
        
        for (pair, start_time) in &self.partitions_timestamps {
            let duration = now.duration_since(*start_time);
            
            if duration >= self.slow_connection_threshold {
                warn!(
                    target: TRACING_TARGET,
                    src = ?pair.0,
                    dst = ?pair.1,
                    duration_ms = ?duration.as_millis(),
                    "Long-lasting network partition detected"
                );
            }
        }
    }
}