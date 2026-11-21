//! Other screenshot tools that take Print Screen for themselves.
//!
//! Two programs can't both own one key. A tool that registers Print Screen
//! as a hotkey before Frame Fang starts leaves Frame Fang's key dead; one
//! that hooks the keyboard instead lets both hear the press, so both capture
//! screens open. Neither explains itself, so Frame Fang looks for the usual
//! suspects among the running programs and names the one it finds.

/// A screenshot tool known to take Print Screen when it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rival {
    /// How the tool calls itself.
    pub name: &'static str,
    /// Its program file names, lower case.
    exes: &'static [&'static str],
}

/// The tools looked for, most common first.
pub const KNOWN: &[Rival] = &[
    Rival { name: "Lightshot", exes: &["lightshot.exe"] },
    Rival { name: "ShareX", exes: &["sharex.exe"] },
    Rival { name: "Greenshot", exes: &["greenshot.exe"] },
    Rival { name: "Snagit", exes: &["snagit32.exe", "snagitcapture.exe"] },
    Rival { name: "PicPick", exes: &["picpick.exe"] },
    Rival { name: "FastStone Capture", exes: &["fscapture.exe"] },
    Rival { name: "Screenpresso", exes: &["screenpresso.exe"] },
    Rival { name: "Flameshot", exes: &["flameshot.exe"] },
];

/// The known tools among `running` (program file names, any case, with or
/// without a folder in front), each once, in the order of `KNOWN`.
pub fn among<'a>(running: impl IntoIterator<Item = &'a str>) -> Vec<&'static Rival> {
    let names: Vec<String> = running.into_iter()
        .map(|path| path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase())
        .collect();
    KNOWN.iter().filter(|r| r.exes.iter().any(|exe| names.iter().any(|n| n == exe))).collect()
}

/// What to tell someone whose capture key is Print Screen while `rival`
/// runs: that both want the key, and the two ways out.
pub fn advice(rival: &Rival) -> String {
    format!(
        "{name} is also using Print Screen, so it may get the key instead, or both may open. \
         Turn off Print Screen in {name}'s settings, or pick another capture key in Frame Fang's Settings.",
        name = rival.name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_tools_by_program_name_in_any_case_or_folder() {
        let running = ["System", r"C:\Program Files (x86)\Skillbrain\Lightshot\Lightshot.exe", "explorer.exe", "ShareX.EXE"];
        let found: Vec<&str> = among(running).iter().map(|r| r.name).collect();
        assert_eq!(found, ["Lightshot", "ShareX"]);
    }

    #[test]
    fn each_tool_is_named_once_and_nothing_else_matches() {
        // Two Snagit processes still make one Snagit.
        assert_eq!(among(["SnagitCapture.exe", "Snagit32.exe"]).len(), 1);
        // A name that only contains a tool's is not it.
        assert!(among(["notlightshot.exe", "lightshot.exe.bak", "framefang.exe"]).is_empty());
        assert!(among([]).is_empty());
    }

    #[test]
    fn every_known_exe_is_lower_case_and_unique() {
        let all: Vec<&str> = KNOWN.iter().flat_map(|r| r.exes.iter().copied()).collect();
        for exe in &all {
            assert_eq!(*exe, exe.to_ascii_lowercase(), "{exe}");
            assert!(exe.ends_with(".exe"), "{exe}");
            assert_eq!(all.iter().filter(|e| e == &exe).count(), 1, "{exe} listed twice");
        }
    }

    #[test]
    fn the_advice_names_the_tool_and_both_ways_out() {
        let text = advice(&KNOWN[0]);
        assert!(text.starts_with("Lightshot is also using Print Screen"));
        assert!(text.contains("Lightshot's settings") && text.contains("another capture key"));
    }
}
