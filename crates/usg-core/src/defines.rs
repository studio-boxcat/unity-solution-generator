use crate::io::read_file;
use crate::paths::join_path;

/// Generate Unity's `UNITY_X_Y_Z`, `UNITY_X_Y`, `UNITY_X`, and `UNITY_X_Y_OR_NEWER`
/// version defines for the given Unity version string (e.g. `6000.2.7f2`).
pub fn generate_version_defines(version: &str) -> Vec<String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 3 {
        return Vec::new();
    }
    let Ok(major) = parts[0].parse::<i32>() else {
        return Vec::new();
    };
    let Ok(minor) = parts[1].parse::<i32>() else {
        return Vec::new();
    };
    let mut patch_str = String::new();
    for ch in parts[2].chars() {
        if !ch.is_ascii_digit() {
            break;
        }
        patch_str.push(ch);
    }
    let Ok(patch) = patch_str.parse::<i32>() else {
        return Vec::new();
    };

    let mut defines = vec![
        format!("UNITY_{}_{}_{}", major, minor, patch),
        format!("UNITY_{}_{}", major, minor),
        format!("UNITY_{}", major),
    ];

    const VERSION_POINTS: &[(i32, i32)] = &[
        (5, 3), (5, 4), (5, 5), (5, 6),
        (2017, 1), (2017, 2), (2017, 3), (2017, 4),
        (2018, 1), (2018, 2), (2018, 3), (2018, 4),
        (2019, 1), (2019, 2), (2019, 3), (2019, 4),
        (2020, 1), (2020, 2), (2020, 3),
        (2021, 1), (2021, 2), (2021, 3),
        (2022, 1), (2022, 2), (2022, 3),
        (2023, 1), (2023, 2), (2023, 3),
        (2024, 1), (2024, 2), (2024, 3),
        (2025, 1), (2025, 2), (2025, 3),
    ];
    for &(maj, min) in VERSION_POINTS {
        if major > maj || (major == maj && minor >= min) {
            defines.push(format!("UNITY_{}_{}_OR_NEWER", maj, min));
        }
    }
    if major >= 6000 {
        for m in 0..=minor {
            defines.push(format!("UNITY_6000_{}_OR_NEWER", m));
        }
    }
    defines
}

/// Stable superset of feature defines emitted by Unity 6.x for managed code.
/// Mirrors `defaultFeatureDefines` in the Swift source — keep in sync.
pub const DEFAULT_FEATURE_DEFINES: &[&str] = &[
    "UNITY_INCLUDE_TESTS",
    "ENABLE_AR", "ENABLE_AUDIO", "ENABLE_CACHING", "ENABLE_CLOTH",
    "ENABLE_EVENT_QUEUE", "ENABLE_MICROPHONE", "ENABLE_MULTIPLE_DISPLAYS",
    "ENABLE_PHYSICS", "ENABLE_TEXTURE_STREAMING", "ENABLE_LZMA",
    "ENABLE_UNITYEVENTS", "ENABLE_VR", "ENABLE_WEBCAM",
    "ENABLE_UNITYWEBREQUEST", "ENABLE_WWW",
    "ENABLE_CLOUD_SERVICES", "ENABLE_CLOUD_SERVICES_ADS",
    "ENABLE_CLOUD_SERVICES_USE_WEBREQUEST", "ENABLE_UNITY_CONSENT",
    "ENABLE_UNITY_CLOUD_IDENTIFIERS",
    "ENABLE_CLOUD_SERVICES_CRASH_REPORTING",
    "ENABLE_CLOUD_SERVICES_NATIVE_CRASH_REPORTING",
    "ENABLE_CLOUD_SERVICES_PURCHASING",
    "ENABLE_CLOUD_SERVICES_ANALYTICS", "ENABLE_CLOUD_SERVICES_BUILD",
    "ENABLE_EDITOR_GAME_SERVICES",
    "ENABLE_UNITY_GAME_SERVICES_ANALYTICS_SUPPORT",
    "ENABLE_CLOUD_LICENSE", "ENABLE_EDITOR_HUB_LICENSE",
    "ENABLE_CLOUD_SERVICES_ENGINE_DIAGNOSTICS",
    "ENABLE_WEBSOCKET_CLIENT",
    "ENABLE_GENERATE_NATIVE_PLUGINS_FOR_ASSEMBLIES_API",
    "ENABLE_DIRECTOR_AUDIO", "ENABLE_DIRECTOR_TEXTURE",
    "ENABLE_MANAGED_JOBS", "ENABLE_MANAGED_TRANSFORM_JOBS",
    "ENABLE_MANAGED_ANIMATION_JOBS", "ENABLE_MANAGED_AUDIO_JOBS",
    "ENABLE_ENGINE_CODE_STRIPPING", "ENABLE_ONSCREEN_KEYBOARD",
    "ENABLE_MANAGED_UNITYTLS", "INCLUDE_DYNAMIC_GI",
    "ENABLE_SCRIPTING_GC_WBARRIERS", "PLATFORM_SUPPORTS_MONO",
    "ENABLE_MARSHALLING_TESTS", "ENABLE_VIDEO",
    "ENABLE_NAVIGATION_OFFMESHLINK_TO_NAVMESHLINK",
    "ENABLE_ACCELERATOR_CLIENT_DEBUGGING", "ENABLE_ACCESSIBILITY",
    "TEXTCORE_1_0_OR_NEWER",
    "TEXTCORE_FONT_ENGINE_1_5_OR_NEWER", "TEXTCORE_TEXT_ENGINE_1_5_OR_NEWER",
    "EDITOR_ONLY_NAVMESH_BUILDER_DEPRECATED",
    "ENABLE_EGL", "ENABLE_NETWORK", "ENABLE_RUNTIME_GI",
    "ENABLE_CRUNCH_TEXTURE_COMPRESSION", "ENABLE_FIREBASE_IDENTIFIERS",
    "UNITY_CAN_SHOW_SPLASH_SCREEN",
    "UNITY_HAS_GOOGLEVR", "UNITY_HAS_TANGO",
    "ENABLE_SPATIALTRACKING", "ENABLE_ETC_COMPRESSION",
    "PLATFORM_EXTENDS_VULKAN_DEVICE", "PLATFORM_HAS_MULTIPLE_SWAPCHAINS",
    "PLATFORM_UPDATES_TIME_OUTSIDE_OF_PLAYER_LOOP",
    "PLATFORM_EXTENDS_VULKAN_PIPELINE_CACHE",
    "PLATFORM_SUPPORTS_SPLIT_GRAPHICS_JOBS",
    "PLATFORM_HAS_ADDITIONAL_API_CHECKS",
    "PLATFORM_HAS_GRAPHICS_JOBS_SUPPORT_CHECK_OVERRIDE",
    "PLATFORM_IMPLEMENTS_INSIGHTS_ANR",
    "PLATFORM_SUPPORTS_INSIGHTS_DEVICE_INFO",
    "ENABLE_ANDROID_ADVERTISING_IDS",
    "PLATFORM_HAS_BUGGY_MSAA_RESOLVE",
    "ENABLE_ANDROID_APP_SET_ID",
    "ENABLE_INSIGHTS_PLATFORM_SPECIFIC_RESOURCES",
    "ENABLE_UNITYADS_RUNTIME", "UNITY_UNITYADS_API",
    "ENABLE_MONO", "NET_STANDARD_2_0", "NET_STANDARD", "NET_STANDARD_2_1",
    "NETSTANDARD", "NETSTANDARD2_1",
    "ENABLE_PROFILER", "ENABLE_UNITY_COLLECTIONS_CHECKS", "ENABLE_BURST_AOT",
    "UNITY_TEAM_LICENSE", "UNITY_PRO_LICENSE",
    "ENABLE_CUSTOM_RENDER_TEXTURE", "ENABLE_DIRECTOR",
    "ENABLE_LOCALIZATION", "ENABLE_SPRITES", "ENABLE_TERRAIN",
    "ENABLE_TILEMAP", "ENABLE_TIMELINE",
    "ENABLE_LEGACY_INPUT_MANAGER",
    "CSHARP_7_OR_LATER", "CSHARP_7_3_OR_NEWER",
];

/// Editor defines that are platform-target-independent. The host-OS suffix
/// (`UNITY_EDITOR_OSX` / `UNITY_EDITOR_WIN` / `UNITY_EDITOR_LINUX`) is added
/// separately via [`editor_host_define`] based on `cfg!(target_os)`.
pub(crate) const EDITOR_DEFINES_BASE: &[&str] = &["UNITY_EDITOR", "UNITY_EDITOR_64"];

/// Defines emitted under `dev`/`editor` configs.
pub(crate) const DEBUG_DEFINES: &[&str] = &["DEBUG", "TRACE", "UNITY_ASSERTIONS"];

/// Host-OS `UNITY_EDITOR_*` define, resolved at run time from `cfg!(target_os)`.
pub(crate) fn editor_host_define() -> &'static str {
    if cfg!(target_os = "windows") {
        "UNITY_EDITOR_WIN"
    } else if cfg!(target_os = "linux") {
        "UNITY_EDITOR_LINUX"
    } else {
        "UNITY_EDITOR_OSX"
    }
}

/// Parse `scriptingDefineSymbols:` from `ProjectSettings.asset`. Returns the
/// union (sorted, deduplicated) across all platforms.
pub fn parse_scripting_defines(project_root: &str) -> Vec<String> {
    let path = join_path(project_root, "ProjectSettings/ProjectSettings.asset");
    let Ok(content) = read_file(&path) else {
        return Vec::new();
    };

    let mut in_section = false;
    let mut all = std::collections::BTreeSet::new();

    for line in content.split('\n') {
        if line.starts_with("  scriptingDefineSymbols:") {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if !line.starts_with("    ") {
            break;
        }
        if let Some(colon) = line.find(':') {
            let value = line[colon + 1..].trim();
            if !value.is_empty() {
                for d in value.split(';') {
                    let d = d.trim();
                    if !d.is_empty() {
                        all.insert(d.to_string());
                    }
                }
            }
        }
    }
    all.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_host_define_matches_target_os() {
        // On the running host, the define must end with the OS suffix that the
        // build machine has — the only one consumers can sensibly wire
        // `#if UNITY_EDITOR_*` against.
        let d = editor_host_define();
        let host_suffix = if cfg!(target_os = "windows") {
            "WIN"
        } else if cfg!(target_os = "linux") {
            "LINUX"
        } else {
            "OSX"
        };
        assert!(d.ends_with(host_suffix), "got {d}, expected suffix {host_suffix}");
    }
}
