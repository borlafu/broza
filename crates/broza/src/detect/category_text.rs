//! The wording `broza explain <category>` prints, one entry per [`Category`].
//!
//! Text only: no logic, no I/O, no formatting. Keeping it apart from
//! [`super::explain`] means the prose can be reviewed as prose, and the module
//! that decides *which* paragraphs to show stays short enough to read.
//!
//! Every entry restates, in the words a non-specialist uses, the row of the
//! category table in `docs/cli-spec.md` §3.3: what the category is, what it is
//! for, and whether removing it is safe at the risk level that table assigns.
//! All text is English (`docs/adr/0005-english-everywhere.md`).

use crate::model::Category;

/// The four pieces of prose Broza keeps for one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CategoryText {
    /// One line for `--short`, printed after the id and the risk label.
    pub(super) summary: &'static str,
    /// What the category is.
    pub(super) what_it_is: &'static str,
    /// What it is for, and why it grows.
    pub(super) what_it_is_for: &'static str,
    /// Whether it is safe to touch, and what Broza itself does.
    pub(super) is_it_safe: &'static str,
}

/// The paragraphs for `category`.
pub(super) const fn text_for(category: Category) -> CategoryText {
    match category {
        Category::UserCache => USER_CACHE,
        Category::BuildCache => BUILD_CACHE,
        Category::IosSimulators => IOS_SIMULATORS,
        Category::Trash => TRASH,
        Category::Snapshots => SNAPSHOTS,
        Category::OldBackups => OLD_BACKUPS,
        Category::UnusedApps => UNUSED_APPS,
        Category::CloudSynced => CLOUD_SYNCED,
        Category::Duplicates => DUPLICATES,
        Category::LargeOldFiles => LARGE_OLD_FILES,
    }
}

/// `user-cache`: regenerable application data (`docs/cli-spec.md` §3.3).
const USER_CACHE: CategoryText = CategoryText {
    summary: "Application caches, logs and incomplete downloads under ~/Library. Regenerated on demand.",
    what_it_is: "Working files applications keep under ~/Library/Caches, plus their logs and the \
half-finished downloads they never cleaned up. None of it is a document you created.",
    what_it_is_for: "A cache buys speed: a browser keeps images, a mail client keeps message \
bodies, a package manager keeps archives it already downloaded. macOS never prunes these \
directories on your behalf, so they only grow.",
    is_it_safe: "Yes. Every application recreates what it needs, at the cost of being a little \
slower the first time. Broza moves the files to quarantine instead of deleting them, so a program \
that behaves oddly afterwards can be put back exactly as it was.",
};

/// `build-cache`: compiler and package-manager output.
const BUILD_CACHE: CategoryText = CategoryText {
    summary: "Build artefacts: DerivedData, Archives, orphan node_modules, __pycache__, .gradle, target/.",
    what_it_is: "Everything a compiler or a package manager wrote so it would not have to do the \
work twice: Xcode DerivedData and Archives, node_modules directories, __pycache__, .gradle, Rust \
target/ directories, and the virtual disk Docker Desktop stores its images in.",
    what_it_is_for: "It makes the second build faster than the first. On a developer's Mac it is \
usually the single largest reclaimable category, and none of it is source code: every byte is \
reproducible from a repository and a build command.",
    is_it_safe: "Yes for the build outputs, which Broza moves to quarantine; the next build \
recreates them. The Docker virtual disk is the exception: Broza only reports its size and points \
at `docker system prune`, because shrinking that file is Docker's job, not a file manager's.",
};

/// `ios-simulators`: Xcode simulator runtimes and devices.
const IOS_SIMULATORS: CategoryText = CategoryText {
    summary: "Unused iOS simulator runtimes and simulated devices installed by Xcode.",
    what_it_is: "Simulator runtimes and simulated devices that Xcode installed, each one a full \
copy of an iOS release together with the data of every app you ran on it.",
    what_it_is_for: "They let you test an app against an iOS version without owning the hardware. \
Xcode keeps every runtime you ever installed, including the ones for iOS versions you no longer \
support.",
    is_it_safe: "Usually, with a look first. Removing a runtime undoes an Xcode download rather \
than losing work — but a simulated device also holds the state of the apps you ran on it, and a \
project that pins an old iOS version will ask you to download the runtime again.",
};

/// `trash`: the Trash of every mounted volume.
const TRASH: CategoryText = CategoryText {
    summary: "The Trash of every mounted volume. Emptying it is irreversible.",
    what_it_is: "The .Trashes and ~/.Trash directories in which Finder parks what you deleted, on \
every volume that has one.",
    what_it_is_for: "It is macOS's own undo for a deletion. Until the Trash is emptied the files \
are still there and still occupy their space, which is why a Mac can report no free space with a \
Trash full of video files.",
    is_it_safe: "It depends on you. Nothing here is needed by the system, but everything here was \
put there by a person, and emptying the Trash is irreversible by definition: Broza cannot \
quarantine what is already the last copy. Read the list before confirming.",
};

/// `snapshots`: APFS local snapshots, sized by nobody.
const SNAPSHOTS: CategoryText = CategoryText {
    summary: "APFS local snapshots (Time Machine, OS update). Managed only via tmutil; sizes not \
reported by macOS.",
    what_it_is: "Point-in-time images of a volume kept on the volume itself. Time Machine takes \
one every hour when it cannot reach its backup disk, and macOS takes one before it installs an \
update.",
    what_it_is_for: "They are what \"enter Time Machine\" browses when the backup disk is not \
attached, and what lets a failed update roll back. They are also why a Mac reports gigabytes of \
\"purgeable\" space: macOS deletes snapshots itself when the disk fills.",
    is_it_safe: "The Time Machine ones, yes, and only through `tmutil`, which is what Broza uses. \
macOS reports no size for a snapshot, so Broza lists them by name and never claims a number it \
cannot measure. Update snapshots (com.apple.os.update-*) are never proposed: a pending update \
needs them.",
};

/// `old-backups`: local iOS device backups.
const OLD_BACKUPS: CategoryText = CategoryText {
    summary: "iPhone and iPad backups under ~/Library/Application Support/MobileSync/Backup.",
    what_it_is: "Full local backups of iPhones and iPads, one directory per device, written by \
Finder or by the older iTunes.",
    what_it_is_for: "They restore a phone after a loss or a replacement. They are also kept \
forever: a device you stopped using years ago still has its backup, and each one can be tens of \
gigabytes.",
    is_it_safe: "With judgement. A backup of a device you still own and still back up to iCloud is \
redundant; a backup of a device that no longer exists may be the only copy of what was on it. \
Broza shows the device name and the date, moves nothing without confirmation, and quarantines \
rather than deletes.",
};

/// `unused-apps`: applications and the libraries they leave behind.
const UNUSED_APPS: CategoryText = CategoryText {
    summary: "Applications not opened past the threshold, plus their leftovers in ~/Library.",
    what_it_is: "Applications in /Applications and ~/Applications that have not been opened for \
longer than the configured threshold, together with the support files, caches and preferences they \
left in ~/Library.",
    what_it_is_for: "Nothing, by definition: that is the finding. The leftovers matter as much as \
the application, because dragging an app to the Trash in Finder leaves its ~/Library directories \
behind, and those are what accumulate.",
    is_it_safe: "Read the list. \"Last opened\" comes from the filesystem and from Spotlight, and \
for system applications macOS often reports nothing at all, so Broza marks those findings as low \
confidence. An application you reinstall is an inconvenience; a licence file deleted with it may \
not be.",
};

/// `cloud-synced`: reported, never deleted (`AGENTS.md` §2.5).
const CLOUD_SYNCED: CategoryText = CategoryText {
    summary: "Files already synced to iCloud Drive, Dropbox, OneDrive or Google Drive. Reported \
only, never deleted.",
    what_it_is: "Local copies of files that a cloud provider already holds: iCloud Drive, Dropbox, \
OneDrive or Google Drive. The provider knows them as synced; your disk knows them as ordinary \
files.",
    what_it_is_for: "They are the copy you work on. Freeing the space means asking the provider to \
evict the local copy and keep the file available on demand, which is a sync operation and not a \
deletion.",
    is_it_safe: "Broza never deletes these. Deleting a synced file with a file manager deletes it \
from the cloud too, and from every other device, which is exactly the kind of mistake this tool \
exists to avoid. Broza reports the size and prints the provider's own instructions for freeing it.",
};

/// `duplicates`: byte-identical files confirmed by hash.
const DUPLICATES: CategoryText = CategoryText {
    summary: "Groups of byte-identical files, confirmed by a full content hash.",
    what_it_is: "Files with identical content in different places, grouped by size, then by their \
first 4 KiB, and only then confirmed with a full BLAKE3 hash. Only confirmed groups are reported.",
    what_it_is_for: "Nothing intentional. They come from copied project directories, from photo \
imports run twice, from archives unpacked next to their originals.",
    is_it_safe: "With judgement. Identical content does not mean interchangeable purpose: two \
copies of one asset may be two projects' dependencies, and a duplicate inside an application \
bundle is part of that application. Broza keeps one copy of each group and quarantines the rest, \
so an over-eager choice can be undone.",
};

/// `large-old-files`: size first, opinion nowhere.
const LARGE_OLD_FILES: CategoryText = CategoryText {
    summary: "Large files not opened for a long time, ranked by size.",
    what_it_is: "Files above the size threshold whose last use is older than the age threshold, \
where last use is the later of the filesystem access time and what Spotlight recorded.",
    what_it_is_for: "Nothing in particular, which is the point: disk images downloaded once, video \
exports, archives of a finished project. They are proposed because of their size, not because \
Broza knows what they are.",
    is_it_safe: "Only you can say. This is the one category where Broza has no opinion about the \
content: it reports where the space went and leaves the decision to you. Anything you accept is \
quarantined, never deleted outright.",
};
