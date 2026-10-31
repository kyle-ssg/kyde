//! "Open in Terminal" off macOS: start the user's own terminal app in a folder.
//!
//! Linux has no single system terminal, so the choice goes, first installed wins:
//! `$TERMINAL`, then the desktop's preferred-terminal setting (Cinnamon, GNOME and MATE keep
//! it in gsettings), then the freedesktop `xdg-terminal-exec`, Debian's `x-terminal-emulator`
//! alternative, and last the common terminals by name. The terminal starts in the folder via
//! its own working-directory flag where it has one, and the process working directory for the
//! rest.
//!
//! Pure + unit-tested apart from [`open_in`], which reads the environment and spawns.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Tried after the user's and the desktop's choice, in this order.
const FALLBACKS: [&str; 6] = [
    "xdg-terminal-exec",
    "x-terminal-emulator",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
];

/// Start the user's terminal in `dir`, detached from Kyde. The error says why none started.
pub(crate) fn open_in(dir: &Path) -> std::io::Result<()> {
    let program = resolve_here().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no terminal app found (set $TERMINAL to your terminal's command)",
        )
    })?;
    let mut child = spawn_in(&program, dir)?;
    // Reap it when it exits (no zombie left behind).
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Start `program` in `dir`, told the folder by its flag and its working directory.
fn spawn_in(program: &Path, dir: &Path) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    Command::new(program)
        .args(dir_args(program, dir))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: closing the shell Kyde was started from must not take the
        // terminal it opened down with it.
        .process_group(0)
        .spawn()
}

/// The terminal [`open_in`] starts, from this process's environment.
pub(crate) fn resolve_here() -> Option<PathBuf> {
    resolve(
        std::env::var("TERMINAL").ok().as_deref(),
        &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        &std::env::var_os("PATH").unwrap_or_default(),
    )
}

/// The first installed terminal of [`candidates`], looked up on `path` (a `PATH` value).
/// `desktops` is `XDG_CURRENT_DESKTOP`, which picks where the desktop's setting lives.
fn resolve(env_terminal: Option<&str>, desktops: &str, path: &OsStr) -> Option<PathBuf> {
    let setting = terminal_schema(desktops).and_then(|schema| desktop_setting(schema, path));
    candidates(env_terminal, setting.as_deref())
        .iter()
        .find_map(|name| find_on(name, path))
}

/// The terminal commands to try, in order, without repeats. `$TERMINAL` and the desktop
/// setting are command lines; their first word is the program.
fn candidates(env_terminal: Option<&str>, setting: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let first_words = [env_terminal, setting]
        .into_iter()
        .flatten()
        .filter_map(|cmd| cmd.split_whitespace().next());
    for name in first_words.chain(FALLBACKS) {
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The gsettings schema holding the preferred terminal on the desktop named by
/// `XDG_CURRENT_DESKTOP` (a `:`-separated list, e.g. `X-Cinnamon` or `ubuntu:GNOME`).
fn terminal_schema(desktops: &str) -> Option<&'static str> {
    let is = |name: &str| {
        desktops.split(':').any(|d| {
            d.eq_ignore_ascii_case(name)
                || d.strip_prefix("X-")
                    .is_some_and(|d| d.eq_ignore_ascii_case(name))
        })
    };
    if is("Cinnamon") {
        Some("org.cinnamon.desktop.default-applications.terminal")
    } else if is("MATE") {
        Some("org.mate.applications-terminal")
    } else if ["GNOME", "Unity", "Budgie", "Pantheon"]
        .iter()
        .any(|d| is(d))
    {
        Some("org.gnome.desktop.default-applications.terminal")
    } else {
        None
    }
}

/// The `exec` key of `schema`, read with the `gsettings` found on `path`. `None` when
/// gsettings or the schema is missing, or the value is empty.
fn desktop_setting(schema: &str, path: &OsStr) -> Option<String> {
    let gsettings = find_on("gsettings", path)?;
    let out = Command::new(gsettings)
        .args(["get", schema, "exec"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    gsettings_string(&String::from_utf8_lossy(&out.stdout))
}

/// A gsettings string as printed (`'gnome-terminal'`), unquoted; `None` when empty.
fn gsettings_string(printed: &str) -> Option<String> {
    let v = printed.trim().trim_matches('\'').trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// `name` as an executable file: itself when it holds a `/`, else the first match on `path`.
fn find_on(name: &str, path: &OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        let p = PathBuf::from(name);
        return runnable(&p).then_some(p);
    }
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| runnable(p))
}

/// The arguments that open `program` in `dir`, by its file name, or else the name of the
/// terminal it links to (Debian's `x-terminal-emulator` alternative is a link to the chosen
/// one). Terminals not listed get none and start in the process working directory, which
/// [`open_in`] sets to `dir` too.
fn dir_args(program: &Path, dir: &Path) -> Vec<OsString> {
    known_dir_args(program, dir)
        .or_else(|| {
            let real = std::fs::canonicalize(program).ok()?;
            known_dir_args(&real, dir)
        })
        .unwrap_or_default()
}

/// [`dir_args`] for a terminal known by `program`'s file name; `None` for any other name.
fn known_dir_args(program: &Path, dir: &Path) -> Option<Vec<OsString>> {
    let joined = |flag: &str| {
        let mut f = OsString::from(flag);
        f.push(dir);
        f
    };
    let name = program.file_name().and_then(OsStr::to_str).unwrap_or("");
    Some(match name {
        "xdg-terminal-exec" => vec![joined("--dir=")],
        "gnome-terminal" | "mate-terminal" | "xfce4-terminal" | "tilix" | "terminator" | "kgx"
        | "foot" => vec![joined("--working-directory=")],
        // Ptyxis reuses its running window unless asked for a new one.
        "ptyxis" => vec!["--new-window".into(), joined("--working-directory=")],
        "konsole" => vec!["--workdir".into(), dir.into()],
        "kitty" => vec!["--directory".into(), dir.into()],
        "alacritty" => vec!["--working-directory".into(), dir.into()],
        "wezterm" => vec!["start".into(), "--cwd".into(), dir.into()],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A fresh, unique temp dir to hold a fake `PATH`.
    fn workdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kyde-terminal-app-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An executable shell script `name` in `dir`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A fake `PATH` laid out like Ubuntu with Cinnamon: gnome-terminal is the Cinnamon
    /// setting, GNOME's setting is xdg-terminal-exec, and x-terminal-emulator is ptyxis.
    fn ubuntu_bin() -> PathBuf {
        let bin = workdir();
        script(
            &bin,
            "gsettings",
            r#"case "$2" in
  org.cinnamon.desktop.default-applications.terminal) echo "'gnome-terminal'" ;;
  org.gnome.desktop.default-applications.terminal) echo "'xdg-terminal-exec'" ;;
  *) echo "No such schema" >&2; exit 1 ;;
esac"#,
        );
        for t in [
            "gnome-terminal",
            "ptyxis",
            "xdg-terminal-exec",
            "x-terminal-emulator",
        ] {
            script(&bin, t, "exit 0");
        }
        bin
    }

    fn name(p: Option<PathBuf>) -> Option<String> {
        p.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    #[test]
    fn candidates_put_the_user_then_the_desktop_first() {
        assert_eq!(
            candidates(Some("kitty --single-instance"), Some("gnome-terminal")),
            [
                "kitty",
                "gnome-terminal",
                "xdg-terminal-exec",
                "x-terminal-emulator",
                "konsole",
                "xfce4-terminal",
                "xterm"
            ]
        );
        assert_eq!(candidates(Some(""), None), FALLBACKS);
        assert_eq!(candidates(None, None), FALLBACKS);
    }

    #[test]
    fn the_desktop_picks_the_settings_schema() {
        let cinnamon = Some("org.cinnamon.desktop.default-applications.terminal");
        let gnome = Some("org.gnome.desktop.default-applications.terminal");
        assert_eq!(terminal_schema("X-Cinnamon"), cinnamon);
        assert_eq!(terminal_schema("Cinnamon"), cinnamon);
        assert_eq!(terminal_schema("ubuntu:GNOME"), gnome);
        assert_eq!(terminal_schema("GNOME"), gnome);
        assert_eq!(terminal_schema("Budgie:GNOME"), gnome);
        assert_eq!(
            terminal_schema("MATE"),
            Some("org.mate.applications-terminal")
        );
        assert_eq!(terminal_schema("KDE"), None);
        assert_eq!(terminal_schema(""), None);
    }

    #[test]
    fn gsettings_strings_lose_their_quotes() {
        assert_eq!(
            gsettings_string("'gnome-terminal'\n").as_deref(),
            Some("gnome-terminal")
        );
        assert_eq!(gsettings_string("''\n"), None);
        assert_eq!(gsettings_string(""), None);
    }

    /// A Cinnamon desktop: its setting (gnome-terminal) wins over xdg-terminal-exec and the
    /// x-terminal-emulator alternative (both ptyxis on Ubuntu 26.04).
    #[test]
    fn cinnamon_opens_its_own_terminal_not_the_alternative() {
        let bin = ubuntu_bin();
        let path = bin.as_os_str();
        assert_eq!(
            name(resolve(None, "X-Cinnamon", path)).as_deref(),
            Some("gnome-terminal")
        );
        // Stock Ubuntu GNOME: its setting is xdg-terminal-exec.
        assert_eq!(
            name(resolve(None, "ubuntu:GNOME", path)).as_deref(),
            Some("xdg-terminal-exec")
        );
        // $TERMINAL beats the desktop; one that isn't installed is skipped.
        assert_eq!(
            name(resolve(Some("ptyxis"), "X-Cinnamon", path)).as_deref(),
            Some("ptyxis")
        );
        assert_eq!(
            name(resolve(Some("no-such-term"), "X-Cinnamon", path)).as_deref(),
            Some("gnome-terminal")
        );
        // A desktop with no gsettings key: the freedesktop launcher, then the alternative.
        assert_eq!(
            name(resolve(None, "KDE", path)).as_deref(),
            Some("xdg-terminal-exec")
        );
        std::fs::remove_file(bin.join("xdg-terminal-exec")).unwrap();
        assert_eq!(
            name(resolve(None, "KDE", path)).as_deref(),
            Some("x-terminal-emulator")
        );
    }

    #[test]
    fn no_desktop_setting_falls_back_by_name() {
        let bin = workdir();
        script(&bin, "xterm", "exit 0");
        // Not executable: not a terminal.
        std::fs::write(bin.join("konsole"), "not a program").unwrap();
        let path = bin.as_os_str();
        assert_eq!(
            name(resolve(None, "X-Cinnamon", path)).as_deref(),
            Some("xterm")
        );
        assert_eq!(resolve(None, "", workdir().as_os_str()), None);
        // An absolute $TERMINAL is used as given.
        let own = script(&workdir(), "my-term", "exit 0");
        assert_eq!(
            resolve(own.to_str(), "", workdir().as_os_str()),
            Some(own.clone())
        );
    }

    #[test]
    fn each_terminal_is_told_the_folder() {
        let dir = Path::new("/tmp/my repo");
        let args = |t: &str| dir_args(Path::new(t), dir);
        assert_eq!(args("/usr/bin/xdg-terminal-exec"), ["--dir=/tmp/my repo"]);
        assert_eq!(
            args("/usr/bin/gnome-terminal"),
            ["--working-directory=/tmp/my repo"]
        );
        assert_eq!(
            args("ptyxis"),
            ["--new-window", "--working-directory=/tmp/my repo"]
        );
        assert_eq!(args("konsole"), ["--workdir", "/tmp/my repo"]);
        assert_eq!(args("wezterm"), ["start", "--cwd", "/tmp/my repo"]);
        assert!(args("xterm").is_empty());
    }

    /// Debian's `x-terminal-emulator` alternative is a link to the chosen terminal (Ptyxis on
    /// Ubuntu 26.04): that terminal's own flags, so Ptyxis opens a new window in the folder.
    /// A link to a terminal not listed gets none, like the terminal itself.
    #[test]
    fn a_linked_terminal_is_told_the_folder_by_its_target() {
        let bin = workdir();
        let dir = Path::new("/tmp/my repo");
        let ptyxis = script(&bin, "ptyxis", "exit 0");
        let alt = bin.join("x-terminal-emulator");
        std::os::unix::fs::symlink(&ptyxis, &alt).unwrap();
        assert_eq!(
            dir_args(&alt, dir),
            ["--new-window", "--working-directory=/tmp/my repo"]
        );
        let other = workdir();
        let own = script(&other, "my-term", "exit 0");
        let alt = other.join("x-terminal-emulator");
        std::os::unix::fs::symlink(&own, &alt).unwrap();
        assert!(dir_args(&alt, dir).is_empty());
        // Not a link, and not a file at all: no flags.
        assert!(dir_args(&own, dir).is_empty());
        assert!(dir_args(Path::new("/no/such/x-terminal-emulator"), dir).is_empty());
    }

    /// The real spawn: a stand-in terminal starts in the folder and is told it.
    #[test]
    fn spawn_starts_the_terminal_in_the_folder() {
        let tmp = workdir();
        let work = tmp.join("work dir");
        std::fs::create_dir_all(&work).unwrap();
        let out = tmp.join("out");
        // Named like gnome-terminal so it also gets the working-directory flag.
        let fake = script(
            &tmp,
            "gnome-terminal",
            &format!("echo \"$PWD|$*\" > '{}'", out.display()),
        );
        // A child another test thread forks while `fake` is being written keeps its write handle
        // until that child execs, and exec refuses a file open for writing (ETXTBSY, "Text file
        // busy"): retry briefly on exactly that.
        let mut child = (0..100)
            .find_map(|_| match spawn_in(&fake, &work) {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
                spawned => Some(spawned),
            })
            .expect("the fake terminal stayed busy")
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().trim(),
            format!("{0}|--working-directory={0}", work.display())
        );
    }
}
