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
                self.assertIsNotNone(gate.RAW_SPAWN.search(code))

    def test_tracked_launch_and_stdio_are_allowed(self):
        for code in (
            "use marathon_process::Command; Command::new(program).spawn();",
            "use std::process::{ExitStatus, Stdio};",
            "libc::kill(pid, libc::SIGTERM);",
            "nix::unistd::getpid();",
        ):
            with self.subTest(code=code):
                self.assertIsNone(gate.RAW_SPAWN.search(code))


if __name__ == "__main__":
    unittest.main()
