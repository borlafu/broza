//! Purgeable space through Foundation's `NSURL` resource values.
//!
//! No `diskutil` command reports purgeable space, and no APFS field holds it.
//! Foundation does, indirectly: macOS tells an application how much room it
//! would have *if* the system purged what it could
//! (`NSURLVolumeAvailableCapacityForImportantUsageKey`) and how much room there
//! is right now (`NSURLVolumeAvailableCapacityKey`). The difference is what
//! macOS is prepared to reclaim on its own
//! (`docs/adr/0002-diskutil-plist-over-diskarbitration.md`).
//!
//! The number is an estimate and Broza labels it as one. It is reported on its
//! own line and never added to free space (`AGENTS.md` §2.7).
//!
//! This is the only file in the crate that calls Objective-C. The `unsafe`
//! in it is two reads of a Foundation string constant and nothing else.

use std::path::Path;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSString, NSURL, NSURLResourceKey,
    NSURLVolumeAvailableCapacityForImportantUsageKey, NSURLVolumeAvailableCapacityKey,
};

use crate::BrozaError;
use crate::ports::SpaceProvider;

/// [`SpaceProvider`] backed by Foundation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NsUrlSpaceProvider;

impl SpaceProvider for NsUrlSpaceProvider {
    fn purgeable_bytes(&self, mount_point: &Path) -> Result<u64, BrozaError> {
        let capacities = volume_capacities(mount_point, &[important_usage_key(), available_key()])?;
        match capacities.as_slice() {
            [important, available] => Ok(compute_purgeable(*important, *available)),
            _ => Err(unavailable(mount_point, "macOS reported no capacity at all")),
        }
    }
}

/// Purgeable bytes from the two capacities macOS reports.
///
/// Both numbers are signed and independently measured, so "important usage"
/// can come back below plain availability — on a volume with nothing to purge,
/// or between two measurements taken microseconds apart. A negative difference
/// is not negative purgeable space; it is zero.
///
/// These are Foundation's numbers, not `diskutil`'s. Foundation's "available"
/// subtracts the reserve macOS keeps for the system and counts the caller's
/// entitlements, so it is routinely a few hundred megabytes below the
/// `CapacityFree` of the same container, and `free_bytes + purgeable_bytes`
/// therefore does not add up to anything `diskutil` prints. Broza reports the
/// container's own `CapacityFree` as free space and this figure only as the
/// purgeable estimate, on its own line, so the two never have to agree.
pub fn compute_purgeable(important_usage: i64, available: i64) -> u64 {
    u64::try_from(important_usage.saturating_sub(available)).unwrap_or(0)
}

/// Ask macOS for `keys` on the volume mounted at `mount_point`, in order.
///
/// Everything Objective-C happens inside one autorelease pool: a scan asks
/// this of every mounted volume, and the temporary `NSURL`, `NSArray` and
/// dictionary of each call would otherwise sit in whatever pool the process
/// happens to have — in a library with no run loop, possibly none at all.
fn volume_capacities(mount_point: &Path, keys: &[&NSURLResourceKey]) -> Result<Vec<i64>, BrozaError> {
    let path =
        mount_point.to_str().ok_or_else(|| unavailable(mount_point, "the mount point is not valid UTF-8"))?;
    autoreleasepool(|_| {
        let values = resource_values(path, keys).map_err(|reason| unavailable(mount_point, &reason))?;
        decode_capacities(&values, keys).map_err(|reason| unavailable(mount_point, &reason))
    })
}

/// The resource dictionary Foundation returns for `path`.
fn resource_values(
    path: &str,
    keys: &[&NSURLResourceKey],
) -> Result<Retained<NSDictionary<NSURLResourceKey, AnyObject>>, String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    url.resourceValuesForKeys_error(&NSArray::from_slice(keys))
        .map_err(|error| error.localizedDescription().to_string())
}

/// The capacities `keys` name, in the order they were asked for.
///
/// Pure: given a dictionary it reads numbers out of it, which is what makes
/// the decoding testable without a volume to point at.
fn decode_capacities(
    values: &NSDictionary<NSURLResourceKey, AnyObject>,
    keys: &[&NSURLResourceKey],
) -> Result<Vec<i64>, String> {
    keys.iter().map(|key| capacity(values, key)).collect()
}

/// One capacity out of the dictionary, as the signed value Foundation uses.
fn capacity(
    values: &NSDictionary<NSURLResourceKey, AnyObject>,
    key: &NSURLResourceKey,
) -> Result<i64, String> {
    let value = values.objectForKey(key).ok_or_else(|| format!("macOS reported no {key}"))?;
    let number = value.downcast_ref::<NSNumber>().ok_or_else(|| format!("{key} was not a number"))?;
    Ok(number.longLongValue())
}

/// The error for a mount point macOS will not answer about.
fn unavailable(mount_point: &Path, reason: &str) -> BrozaError {
    BrozaError::Other(format!("purgeable space for {} is unavailable: {reason}", mount_point.display()))
}

/// Key for the room an application would have after macOS purges what it can.
#[allow(unsafe_code)]
fn important_usage_key() -> &'static NSURLResourceKey {
    // SAFETY: an immutable `NSString` constant exported by Foundation, which is
    // linked into every process on macOS. It is initialised before `main` and
    // never written to, so reading it cannot race with anything.
    unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey }
}

/// Key for the room that is free right now.
#[allow(unsafe_code)]
fn available_key() -> &'static NSURLResourceKey {
    // SAFETY: as above — a Foundation string constant, immutable and always
    // present in the process.
    unsafe { NSURLVolumeAvailableCapacityKey }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2_foundation::{
        NSDictionary, NSNumber, NSString, NSURLResourceKey, NSURLVolumeTotalCapacityKey, NSValue,
    };

    use super::{
        NsUrlSpaceProvider, available_key, compute_purgeable, decode_capacities, important_usage_key,
        volume_capacities,
    };
    use crate::ports::SpaceProvider;

    /// The untyped dictionary entry a Foundation object becomes.
    fn object(value: Retained<NSObject>) -> Retained<AnyObject> {
        value.into()
    }

    /// A resource dictionary as Foundation would have returned it.
    fn dictionary(
        entries: &[(&NSURLResourceKey, i64)],
    ) -> Retained<NSDictionary<NSURLResourceKey, AnyObject>> {
        let keys: Vec<&NSURLResourceKey> = entries.iter().map(|(key, _)| *key).collect();
        let values: Vec<Retained<AnyObject>> = entries
            .iter()
            .map(|(_, value)| {
                let number: Retained<NSValue> = Retained::into_super(NSNumber::numberWithLongLong(*value));
                object(Retained::into_super(number))
            })
            .collect();
        NSDictionary::from_retained_objects(&keys, &values)
    }

    #[test]
    fn purgeable_space_is_what_macos_would_free_beyond_what_is_already_free() {
        assert_eq!(compute_purgeable(100, 40), 60);
        assert_eq!(compute_purgeable(100, 100), 0);
    }

    #[test]
    fn an_estimate_below_what_is_already_free_is_no_purgeable_space_at_all() {
        assert_eq!(compute_purgeable(40, 100), 0, "a negative difference is zero, never a huge u64");
        assert_eq!(compute_purgeable(-1, 0), 0);
        assert_eq!(compute_purgeable(i64::MIN, i64::MAX), 0, "the subtraction must not overflow");
    }

    #[test]
    fn the_largest_plausible_estimate_survives_the_conversion() {
        assert_eq!(compute_purgeable(i64::MAX, 0), u64::try_from(i64::MAX).unwrap_or(0));
    }

    #[test]
    fn the_capacities_come_back_in_the_order_they_were_asked_for() {
        let keys = [important_usage_key(), available_key()];
        let values = dictionary(&[(keys[0], 900), (keys[1], 400)]);

        let decoded = decode_capacities(&values, &keys).unwrap_or_else(|reason| panic!("{reason}"));

        assert_eq!(decoded, vec![900, 400]);
        assert_eq!(compute_purgeable(decoded[0], decoded[1]), 500);
    }

    #[test]
    fn a_key_macos_did_not_answer_is_named_in_the_failure() {
        let keys = [important_usage_key(), available_key()];
        let values = dictionary(&[(keys[0], 900)]);

        let reason = decode_capacities(&values, &keys).err().unwrap_or_default();

        assert!(reason.contains("AvailableCapacity"), "{reason}");
    }

    #[test]
    fn a_value_that_is_not_a_number_is_a_failure_and_not_a_panic() {
        let key = important_usage_key();
        let text = object(Retained::into_super(NSString::from_str("not a number")));
        let values = NSDictionary::from_retained_objects(&[key], &[text]);

        let reason = decode_capacities(&values, &[key]).err().unwrap_or_default();

        assert!(reason.contains("was not a number"), "{reason}");
    }

    /// Reads a real volume, so it is not part of the normal suite
    /// (`AGENTS.md` §7).
    #[test]
    #[ignore = "asks the running system about a path; run with --ignored"]
    fn a_path_that_does_not_exist_is_an_error_naming_the_path() {
        let err = NsUrlSpaceProvider.purgeable_bytes(Path::new("/definitely/not/a/volume")).err();

        let Some(error) = err else { panic!("expected an error") };
        assert!(error.to_string().contains("/definitely/not/a/volume"), "{error}");
    }

    /// Talks to the running system, so it is not part of the normal suite.
    ///
    /// Run both ignored tests of this module with:
    ///
    /// ```text
    /// cargo test --workspace --all-features -- --ignored nsurl_space
    /// ```
    ///
    /// This one asserts the only invariant that holds on every Mac: macOS
    /// cannot offer to purge more than the volume can hold. The exact figure
    /// changes minute by minute, so there is nothing else to pin it to.
    #[test]
    #[ignore = "reads the real boot volume; run with --ignored"]
    fn purgeable_space_on_the_real_boot_volume_is_smaller_than_the_volume() {
        let root = Path::new("/");
        let total =
            volume_capacities(root, &[total_capacity_key()]).unwrap_or_else(|error| panic!("{error}"));

        let purgeable = NsUrlSpaceProvider.purgeable_bytes(root).unwrap_or_else(|error| panic!("{error}"));

        let total = u64::try_from(total[0]).unwrap_or(0);
        assert!(total > 0, "the boot volume has a capacity");
        assert!(purgeable < total, "purgeable {purgeable} must be below the capacity {total}");
    }

    /// The total-capacity key, for the ignored test above.
    #[allow(unsafe_code)]
    fn total_capacity_key() -> &'static objc2_foundation::NSURLResourceKey {
        // SAFETY: a Foundation string constant, immutable and always present in
        // the process, like the keys the provider itself uses.
        unsafe { NSURLVolumeTotalCapacityKey }
    }
}
