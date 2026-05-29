//! Build-variant vocabulary: the target platform + configuration enums that
//! identify one `.csproj`/`.sln` variant. Kept in a leaf module (depended on by
//! `solution_generator`, `csproj_render`, and `typecheck`) so those consumers
//! don't reach back into the orchestrator for core types.

use crate::lockfile::RefCategory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPlatform {
    Ios,
    Android,
    Osx,
    Windows,
}

impl BuildPlatform {
    /// All variants in canonical order. Iterating this is the supported way to
    /// walk the variant set; matches in this file are exhaustive so adding a
    /// variant fails to compile until everywhere is updated.
    pub const ALL: &'static [BuildPlatform] = &[
        BuildPlatform::Ios,
        BuildPlatform::Android,
        BuildPlatform::Osx,
        BuildPlatform::Windows,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ios" => BuildPlatform::Ios,
            "android" => BuildPlatform::Android,
            "osx" => BuildPlatform::Osx,
            "windows" => BuildPlatform::Windows,
            _ => return None,
        })
    }

    /// Unity's `includePlatforms` value. Used for platform-filter matching.
    pub fn unity_platform_name(self) -> &'static str {
        match self {
            BuildPlatform::Ios => "iOS",
            BuildPlatform::Android => "Android",
            BuildPlatform::Osx => "macOSStandalone",
            BuildPlatform::Windows => "WindowsStandalone",
        }
    }

    pub fn platform_defines(self) -> &'static [&'static str] {
        match self {
            BuildPlatform::Ios => &["UNITY_IOS", "UNITY_IPHONE"],
            BuildPlatform::Android => &["UNITY_ANDROID"],
            BuildPlatform::Osx => &["UNITY_STANDALONE", "UNITY_STANDALONE_OSX"],
            BuildPlatform::Windows => &["UNITY_STANDALONE", "UNITY_STANDALONE_WIN"],
        }
    }

    /// PlaybackEngines subdir under the Unity install. `None` for standalone-mac
    /// — that variant uses the shared `PlaybackStandalone` ref category populated
    /// once per install rather than a target-specific subdir.
    pub fn playback_ref_category(self) -> RefCategory {
        match self {
            BuildPlatform::Ios => RefCategory::PlaybackIos,
            BuildPlatform::Android => RefCategory::PlaybackAndroid,
            BuildPlatform::Osx => RefCategory::PlaybackStandalone,
            BuildPlatform::Windows => RefCategory::PlaybackWindows,
        }
    }

    /// Ref categories to resolve for this target, in canonical merge order
    /// (first-wins on name dedup). `PlaybackStandalone` always participates —
    /// it holds engine DLLs shared across desktop targets — and the
    /// target-specific playback category is appended unless it *is* standalone.
    /// Shared by `csproj_render` (HintPath references) and `typecheck` (csc
    /// `/reference:` args) so both resolve the same set the same way.
    pub(crate) fn ref_categories(self, is_editor: bool) -> Vec<RefCategory> {
        let mut cats = vec![RefCategory::Engine];
        if is_editor {
            cats.push(RefCategory::Editor);
        }
        cats.push(RefCategory::PlaybackStandalone);
        let target_cat = self.playback_ref_category();
        if target_cat != RefCategory::PlaybackStandalone {
            cats.push(target_cat);
        }
        cats.push(RefCategory::Project);
        cats.push(RefCategory::Netstandard);
        cats
    }
}

impl std::fmt::Display for BuildPlatform {
    /// CLI / config string. Round-trips with [`BuildPlatform::parse`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BuildPlatform::Ios => "ios",
            BuildPlatform::Android => "android",
            BuildPlatform::Osx => "osx",
            BuildPlatform::Windows => "windows",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildConfig {
    Editor,
    Dev,
    Prod,
}

impl BuildConfig {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "editor" => BuildConfig::Editor,
            "dev" => BuildConfig::Dev,
            "prod" => BuildConfig::Prod,
            _ => return None,
        })
    }
}

impl std::fmt::Display for BuildConfig {
    /// CLI / config string ("editor", "dev", "prod"). Round-trips with [`BuildConfig::parse`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BuildConfig::Editor => "editor",
            BuildConfig::Dev => "dev",
            BuildConfig::Prod => "prod",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_round_trips() {
        assert_eq!(BuildPlatform::parse("windows"), Some(BuildPlatform::Windows));
        assert_eq!(BuildPlatform::Windows.to_string(), "windows");
    }

    #[test]
    fn windows_platform_metadata() {
        let p = BuildPlatform::Windows;
        assert_eq!(p.unity_platform_name(), "WindowsStandalone");
        assert_eq!(p.platform_defines(), &["UNITY_STANDALONE", "UNITY_STANDALONE_WIN"]);
        assert_eq!(p.playback_ref_category(), RefCategory::PlaybackWindows);
    }

    #[test]
    fn playback_ref_category_per_target() {
        assert_eq!(BuildPlatform::Ios.playback_ref_category(), RefCategory::PlaybackIos);
        assert_eq!(BuildPlatform::Android.playback_ref_category(), RefCategory::PlaybackAndroid);
        assert_eq!(BuildPlatform::Osx.playback_ref_category(), RefCategory::PlaybackStandalone);
        assert_eq!(BuildPlatform::Windows.playback_ref_category(), RefCategory::PlaybackWindows);
    }

    #[test]
    fn all_covers_every_variant() {
        // If a new variant is added, `playback_ref_category`'s exhaustive match
        // forces it to be addressed and this length check forces ALL to be
        // updated. (The match alone won't fire — ALL is a const array literal.)
        assert_eq!(BuildPlatform::ALL.len(), 4);
    }
}
