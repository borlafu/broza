//! Small facts about BSD device names, shared by the assembly.

/// Prefix every BSD disk name starts with.
const DISK_PREFIX: &str = "disk";
/// Separator between a disk and its slice (`disk0s2`).
const SLICE_SEPARATOR: char = 's';

/// The whole disk a partition belongs to: `disk0s2` is on `disk0`.
///
/// A name this does not recognise is returned unchanged, so it can only ever
/// fail to match a disk, never match the wrong one.
pub(crate) fn whole_disk_of(partition: &str) -> &str {
    let Some(rest) = partition.strip_prefix(DISK_PREFIX) else { return partition };
    match rest.find(SLICE_SEPARATOR) {
        Some(offset) => &partition[..DISK_PREFIX.len() + offset],
        None => partition,
    }
}

/// Sort key that orders `disk9` before `disk10`, unlike a string comparison.
pub(crate) fn device_order(id: &str) -> Vec<u64> {
    id.split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

/// `items` in BSD order, which is the order two runs have to agree on.
pub(crate) fn ordered_by_device<T>(mut items: Vec<T>, key: impl Fn(&T) -> &str) -> Vec<T> {
    items.sort_by_key(|item| device_order(key(item)));
    items
}

/// `value` unless it is zero, in which case `fallback`.
pub(crate) fn nonzero_or(value: u64, fallback: u64) -> u64 {
    if value == 0 { fallback } else { value }
}

#[cfg(test)]
mod tests {
    use super::{device_order, nonzero_or, ordered_by_device, whole_disk_of};

    #[test]
    fn a_partition_belongs_to_the_disk_its_name_starts_with() {
        assert_eq!(whole_disk_of("disk0s2"), "disk0");
        assert_eq!(whole_disk_of("disk12s3s1"), "disk12");
        assert_eq!(whole_disk_of("disk3"), "disk3");
        assert_eq!(whole_disk_of("nvme0n1"), "nvme0n1", "a name Broza cannot split stays whole");
    }

    #[test]
    fn devices_are_ordered_by_number_and_not_alphabetically() {
        assert!(device_order("disk9") < device_order("disk10"));
        assert!(device_order("disk3s2") < device_order("disk3s10"));
        assert!(device_order("disk3") < device_order("disk3s1"));
    }

    #[test]
    fn ordering_a_list_puts_the_lower_numbers_first() {
        let ordered = ordered_by_device(vec!["disk10s1", "disk2", "disk10"], |id| id);

        assert_eq!(ordered, vec!["disk2", "disk10", "disk10s1"]);
    }

    #[test]
    fn a_zero_size_falls_back_to_what_the_partition_map_reported() {
        assert_eq!(nonzero_or(0, 7), 7);
        assert_eq!(nonzero_or(5, 7), 5);
    }
}
