use std::env;
use std::process::{Command, Stdio};
use std::io::{self, Write};
use std::borrow::Cow;
use std::path::{Path, Component, PrefixComponent, Prefix};

#[macro_use]
extern crate lazy_static;
extern crate regex;

use regex::bytes::Regex;


fn get_drive_letter(pc: &PrefixComponent) -> Option<String> {
    let drive_byte = match pc.kind() {
        Prefix::VerbatimDisk(d) => Some(d),
        Prefix::Disk(d) => Some(d),
        _ => None
    };
    drive_byte.map(|drive_letter| {
        String::from_utf8(vec![drive_letter])
            .expect(&format!("Invalid drive letter: {}", drive_letter))
            .to_lowercase()
    })
}

fn get_prefix_for_drive(drive: &str) -> String {
    // todo - lookup mount points
    format!("/mnt/{}", drive)
}

fn translate_path_to_unix(argument: String) -> String {
    {
        let (argname, arg) = if argument.starts_with("--")
            && argument.contains('=') {
            let parts: Vec<&str> = argument
                .splitn(2, '=')
                .collect();
            (format!("{}=", parts[0]), parts[1])
        } else {
            ("".to_owned(), argument.as_ref())
        };
        let win_path = Path::new(arg);
        if win_path.is_absolute() || win_path.exists() {
            let wsl_path: String = win_path.components().fold(
                String::new(), |mut acc, c| {
                    match c {
                        Component::Prefix(prefix_comp) => {
                            let d = get_drive_letter(&prefix_comp).expect(
                                &format!("Cannot handle path {:?}",
                                         win_path));
                            acc.push_str(&get_prefix_for_drive(&d));
                        }
                        Component::RootDir => {}
                        _ => {
                            let d = c.as_os_str().to_str()
                                .expect(
                                    &format!("Cannot represent path {:?}",
                                             win_path))
                                .to_owned();
                            if !acc.is_empty() && !acc.ends_with('/') {
                                acc.push('/');
                            }
                            acc.push_str(&d);
                        }
                    };
                    acc
                });
            return format!("{}{}", &argname, &wsl_path);
        }
    }
    argument
}

fn translate_path_to_win(line: &[u8]) -> Cow<[u8]> {
    lazy_static! {
        static ref WSLPATH_RE: Regex =
            Regex::new(r"(?m-u)/mnt/(?P<drive>[A-Za-z])(?P<path>/\S*)")
                .expect("Failed to compile WSLPATH regex");
    }
    WSLPATH_RE.replace_all(line, &b"${drive}:${path}"[..])
}

fn shell_escape(arg: String) -> String {
    // ToDo: This really only handles arguments with spaces and newlines.
    // More complete shell escaping is required for the general case.
    if arg.contains(" ") {
        return vec![
            String::from("\""),
            arg,
            String::from("\"")].join("");
    }
    arg.replace("\n", "$'\n'")
}

fn unquote(s: String) -> String {
    if s.starts_with('"') {
        return s.get(1..(s.len() - 1)).map(String::from).unwrap_or(s);
    }
    s
}

fn resolve_actual_win_path(win_path: &Path) -> Option<String> {
    ["", "CMD", "EXE"]
        .iter()
        .map(|ext| win_path.with_extension(ext))
        .map(|p| {
            println!("path {}", p.to_str().unwrap_or(""));
            p
        })
        .find(|p| p.exists())?
        .canonicalize().ok()?
        .to_str()
        .map(String::from)
}

fn translate_git_editor(editor: String) -> String {
    let editor_parts: Vec<&str> = editor.splitn(2, " ").collect();
    let unquoted_editor_cmd = unquote(String::from(editor_parts[0]));
    let win_path = Path::new(&unquoted_editor_cmd);
    let unix_path = resolve_actual_win_path(win_path)
        .map(|actual_win_path| translate_path_to_unix(actual_win_path))
        .unwrap_or(String::from(editor_parts[0]));

    return [
        unix_path,
        String::from(editor_parts[1])
    ].join(" ");
}

fn main() {
    let mut cmd_args = Vec::new();
    let cwd_unix = translate_path_to_unix(env::current_dir().unwrap().to_string_lossy().into_owned());
    let mut git_args: Vec<String> = vec![String::from("git")];
    let git_cmd: String;

    // process git command arguments
    git_args.extend(env::args().skip(1)
        .map(translate_path_to_unix));

    git_cmd = git_args.join(" ");
    cmd_args = git_args;

    // setup stdin/stdout
    let stdin_mode = if env::args().last().unwrap() == "--version" {
        // For some reason, the git subprocess seems to hang, waiting for
        // input, when VS Code 1.17.2 tries to detect if `git --version` works
        // on Windows 10 1709 (specifically, in `findSpecificGit` in the
        // VS Code source file `extensions/git/src/git.ts`).
        // To workaround this, we only pass stdin to the git subprocess
        // for all other commands, but not for the initial `--version` check.
        // Stdin is needed for example when commiting, where the commit
        // message is passed on stdin.
        Stdio::inherit()
    } else {
        Stdio::inherit()
    };

    // setup the git subprocess launched inside WSL
    let mut git_proc_setup = Command::new("wsl");
    git_proc_setup.args(&cmd_args)
        .stdin(stdin_mode);
    let status;

    match env::var("GIT_EDITOR") {
        Ok(val) => {
            git_proc_setup.env("GIT_EDITOR", translate_git_editor(val));
        }
        _ => {}
    }

    // add git commands that must use translate_path_to_win
    const TRANSLATED_CMDS: &[&str] = &["rev-parse", "remote"];

    let translate_output =
        env::args().skip(1).position(|arg| TRANSLATED_CMDS.iter().position(|&tcmd| tcmd == arg).is_some()).is_some();

    if translate_output {
        // run the subprocess and capture its output
        let git_proc = git_proc_setup.stdout(Stdio::piped())
            .spawn()
            .expect(&format!("Failed to execute command '{}'", &git_cmd));
        let output = git_proc
            .wait_with_output()
            .expect(&format!("Failed to wait for git call '{}'", &git_cmd));
        status = output.status;
        let output_bytes = output.stdout;
        let mut stdout = io::stdout();
        stdout
            .write_all(&translate_path_to_win(&output_bytes))
            .expect("Failed to write git output");
        stdout.flush().expect("Failed to flush output");
    } else {
        // run the subprocess without capturing its output
        // the output of the subprocess is passed through unchanged
        status = git_proc_setup
            .stdout(Stdio::inherit())
            .status()
            .expect(&format!("Failed to execute command '{}'", &git_cmd));
    }

    // forward any exit code
    if let Some(exit_code) = status.code() {
        std::process::exit(exit_code);
    }
}


#[test]
fn win_to_unix_path_trans() {
    assert_eq!(
        translate_path_to_unix("d:\\test\\file.txt".to_string()),
        "/mnt/d/test/file.txt");
    assert_eq!(
        translate_path_to_unix("C:\\Users\\test\\a space.txt".to_string()),
        "/mnt/c/Users/test/a space.txt");
}

#[test]
fn unix_to_win_path_trans() {
    assert_eq!(
        &*translate_path_to_win(b"/mnt/d/some path/a file.md"),
        b"d:/some path/a file.md");
    assert_eq!(
        &*translate_path_to_win(b"origin  /mnt/c/path/ (fetch)"),
        b"origin  c:/path/ (fetch)");
    let multiline = b"mirror  /mnt/c/other/ (fetch)\nmirror  /mnt/c/other/ (push)\n";
    let multiline_result = b"mirror  c:/other/ (fetch)\nmirror  c:/other/ (push)\n";
    assert_eq!(
        &*translate_path_to_win(&multiline[..]),
        &multiline_result[..]);
}

#[test]
fn no_path_translation() {
    assert_eq!(
        &*translate_path_to_win(b"/mnt/other/file.sh"),
        b"/mnt/other/file.sh");
}

#[test]
fn relative_path_translation() {
    assert_eq!(
        translate_path_to_unix(".\\src\\main.rs".to_string()),
        "./src/main.rs");
}

#[test]
fn long_argument_path_translation() {
    assert_eq!(
        translate_path_to_unix("--file=C:\\some\\path.txt".to_owned()),
        "--file=/mnt/c/some/path.txt");
}
