#!/usr/bin/env python3
"""Exercise forbidden launch syntax and the ownership boundary."""
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location(
    "check_process_spawns", Path(__file__).with_name("check_process_spawns.py")
)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class ProcessSpawnGate(unittest.TestCase):
    def test_raw_launches_are_rejected(self):
        for code in (
            "use std::process::*; fn start() { Command::new(\"sleep\").spawn(); }",
            "use tokio::{process::{self, *}, time};",
            "std::process::Command::new(program)",
            "use tokio::process::{Child, Command as RawCommand};",
            "use std::{process as raw, env};",
            "libc::system(cmd);",
            "libc::popen(cmd, mode);",
            "libc::execl(path, arg, end);",
            "libc::execvp(file, argv);",
            "libc::execveat(fd, path, argv, env, flags);",
            "use libc::{getpid, execvpe as run};",
            "nix::unistd::fork();",
            "use nix::{unistd::{getpid, fork as clone}, sys};",
            "nix::spawn::posix_spawnp(executable, actions, attributes, argv, env);",
        ):
            with self.subTest(code=code):
                self.assertTrue(gate.spawn_lines(code))

    def test_tracked_launch_and_stdio_are_allowed(self):
        for code in (
            "use marathon_process::Command; Command::new(program).spawn();",
            "use std::process::{ExitStatus, Stdio};",
            "libc::kill(pid, libc::SIGTERM);",
            "nix::unistd::getpid();",
        ):
            with self.subTest(code=code):
                self.assertFalse(gate.spawn_lines(code))


    def test_native_glob_imports_do_not_hide_spawn_operations(self):
        for code in (
            "use nix::unistd::*;\nunsafe { fork() };",
            "use libc::*;\nunsafe { execvp(hello, argv) };",
            "use nix::{unistd::*, spawn::PosixSpawnAttr};\nposix_spawn(&mut pid);",
        ):
            with self.subTest(code=code):
                self.assertEqual(gate.spawn_lines(code), [2])

    def test_parenthesized_import_and_qualified_spawn(self):
        self.assertEqual(gate.spawn_lines("use tokio::{io, process as p};\nlibc::fork();"), [1, 2])

    def test_comments_and_literals_do_not_match_or_hide_code(self):
        code = """// std::process::Command
/* old code /* use libc::*; */ execvp(hello, argv); */
let log = r#"/* std::process::Command -> ssh */ "#;
let url = "https://fixture.invalid";
let keep = false; // libc::fork()
tokio::process::Command::new("fixture");
fork(); // no imported libc or nix helper in this module
"""
        self.assertEqual(gate.spawn_lines(code), [6])


if __name__ == "__main__":
    unittest.main()
