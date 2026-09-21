//! Hard-link bookkeeping: every `(device, inode)` pair counts once per walk.
//!
//! A file with several names would otherwise be added to the total once per name,
//! and Broza would claim more reclaimable space than the disk has (`AGENTS.md`
//! §2.7, `docs/cli-spec.md` §4.2). The registry is the shared state the parallel
//! walker needs; it is sharded so that the threads walking different directories
//! rarely contend on the same lock.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

/// Number of independent locks the registry is split into.
///
/// A power of two so the shard index is a mask, and large enough that the walker's
/// threads (one per core) seldom meet on the same one.
const SHARD_COUNT: usize = 64;
/// Odd multiplier that spreads consecutive inode numbers across the shards.
const SHARD_MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// The set of `(device, inode)` pairs already counted in one walk.
#[derive(Debug)]
pub(crate) struct LinkRegistry {
    /// One lock-protected set per shard.
    shards: Vec<Mutex<HashSet<(u64, u64)>>>,
}

impl LinkRegistry {
    /// An empty registry.
    pub(crate) fn new() -> Self {
        Self { shards: (0..SHARD_COUNT).map(|_| Mutex::new(HashSet::new())).collect() }
    }

    /// Claim `(device, inode)`; `true` only for the first caller that sees it.
    ///
    /// A `false` answer means another name for the same file was already counted,
    /// so this one contributes no bytes.
    pub(crate) fn claim(&self, device: u64, inode: u64) -> bool {
        let index = Self::shard_of(device, inode);
        match self.shards.get(index) {
            Some(shard) => shard.lock().unwrap_or_else(PoisonError::into_inner).insert((device, inode)),
            // Unreachable: `shard_of` is taken modulo the shard count.
            None => true,
        }
    }

    /// Shard that owns `(device, inode)`.
    fn shard_of(device: u64, inode: u64) -> usize {
        let mixed = inode.wrapping_mul(SHARD_MIX) ^ device.wrapping_mul(SHARD_MIX.rotate_left(17));
        usize::try_from(mixed % SHARD_COUNT as u64).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::LinkRegistry;

    #[test]
    fn the_first_claim_of_an_inode_wins_and_later_ones_lose() {
        let registry = LinkRegistry::new();

        assert!(registry.claim(1, 42));
        assert!(!registry.claim(1, 42));
    }

    #[test]
    fn the_same_inode_on_another_device_is_another_entry() {
        let registry = LinkRegistry::new();

        assert!(registry.claim(1, 42));
        assert!(registry.claim(2, 42));
    }

    #[test]
    fn claims_from_many_threads_elect_exactly_one_winner_per_inode() {
        let registry = LinkRegistry::new();
        let winners = AtomicU64::new(0);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for inode in 0..500 {
                        if registry.claim(7, inode) {
                            winners.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });

        assert_eq!(winners.load(Ordering::Relaxed), 500);
    }
}
