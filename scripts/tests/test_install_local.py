"""scripts/install-local.py against a temporary HOME and a fake checkout.

Run: python3 -m unittest discover -s scripts/tests
Nothing here touches the real ~/.local, Codex settings or whisper models: the
release binaries, FFmpeg library, whisper-cli and models are small fakes.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install-local.py"
NAMES = ["ferrocut", "ferrocut-mcp", "ferrocut-perceive", "ferrocut-deliver"]
CLI = Path("third_party/whisper.cpp/build/bin/whisper-cli")
MODELS = Path("third_party/whisper-models")


def executable(path, text="#!/bin/sh\nexit 0\n"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


class Checkout:
    """A git checkout with fake release binaries and FFmpeg."""

    def __init__(self, base):
        self.root = base / "checkout"
        for name in NAMES:
            executable(self.root / "target/release" / name)
        lib = self.root / "third_party/ffmpeg-lgpl/lib"
        lib.mkdir(parents=True)
        (lib / "libavutil.so").write_text("fake")
        (self.root / "LICENSE").write_text("Apache-2.0")
        git = ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(git + ["-C", str(self.root), "add", "LICENSE"], check=True)
        subprocess.run(git + ["-C", str(self.root), "commit", "-q", "-m", "fake"], check=True)

    def whisper(self, cli=True, models=("ggml-small.bin",)):
        if cli:
            executable(self.root / CLI)
        for name in models:
            (self.root / MODELS / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / MODELS / name).write_text("fake model " + name)


class InstallLocalTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.home = base / "home"
        self.home.mkdir()
        self.checkout = Checkout(base)

    def tearDown(self):
        self.tmp.cleanup()

    def install(self, *args):
        env = dict(os.environ, HOME=str(self.home))
        out = subprocess.run([sys.executable, str(INSTALLER), "--root", str(self.checkout.root), *args],
                             env=env, capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        summary = json.loads(out.stdout[:out.stdout.rindex("}") + 1])
        return summary, Path(summary["runtime"])

    def whisper_entries(self, runtime):
        return sorted(str(p.relative_to(runtime)) for p in runtime.rglob("*")
                      if "whisper" in str(p.relative_to(runtime)) and (p.is_symlink() or p.is_file()))

    def test_no_whisper_installs_as_before(self):
        summary, runtime = self.install()
        self.assertEqual(summary["whisper"], {"cli": None, "models": []})
        self.assertEqual(self.whisper_entries(runtime), [])
        self.assertTrue((runtime / "target/release/ferrocut").is_file())
        self.assertTrue((runtime / "third_party/ffmpeg-lgpl/lib/libavutil.so").is_file())
        record = json.loads((self.home / ".local/share/ferrocut/install.json").read_text())
        self.assertEqual(record["whisper"], {"cli": None, "models": []})

    def test_links_exactly_the_discoverable_whisper_files(self):
        self.checkout.whisper(models=("ggml-small.bin", "ggml-large-v3.bin"))
        (self.checkout.root / "third_party/whisper.cpp/README.md").write_text("source tree")
        summary, runtime = self.install()
        root = self.checkout.root.resolve()
        self.assertEqual(summary["whisper"], {"cli": str(root / CLI),
                                              "models": [str(root / MODELS / "ggml-small.bin")]})
        self.assertEqual(self.whisper_entries(runtime), [str(CLI), str(MODELS / "ggml-small.bin")])
        for rel in (CLI, MODELS / "ggml-small.bin"):
            path = runtime / rel
            self.assertTrue(path.is_symlink())
            self.assertEqual(Path(os.readlink(path)), root / rel)  # absolute, into the checkout
        # Linked, not copied: the model bytes live only in the checkout.
        self.assertFalse((runtime / MODELS / "ggml-large-v3.bin").exists())

    def test_check_reports_links_and_writes_nothing(self):
        self.checkout.whisper()
        summary, runtime = self.install("--check")
        self.assertFalse(summary["write"])
        self.assertIsNotNone(summary["whisper"]["cli"])
        self.assertFalse(runtime.exists())
        self.assertFalse((self.home / ".local").exists())

    def test_non_executable_cli_is_not_linked(self):
        self.checkout.whisper(cli=False)
        (self.checkout.root / CLI).parent.mkdir(parents=True)
        (self.checkout.root / CLI).write_text("not executable")
        summary, runtime = self.install()
        self.assertIsNone(summary["whisper"]["cli"])
        self.assertEqual(self.whisper_entries(runtime), [str(MODELS / "ggml-small.bin")])

    def test_reinstall_refreshes_links_and_keeps_other_versions(self):
        self.checkout.whisper(models=("ggml-small.bin", "ggml-medium.bin"))
        _, runtime = self.install()
        old = self.home / ".local/share/ferrocut/versions/older-version"
        old.mkdir(parents=True)
        (old / "keep").write_text("untouched")
        # The medium model leaves the checkout; reinstalling the same build
        # drops its link and keeps the others.
        (self.checkout.root / MODELS / "ggml-medium.bin").unlink()
        time.sleep(1.1)  # the installer's backup directory is named by the second
        _, again = self.install()
        self.assertEqual(again, runtime)
        self.assertEqual(self.whisper_entries(runtime), [str(CLI), str(MODELS / "ggml-small.bin")])
        self.assertEqual((old / "keep").read_text(), "untouched")


if __name__ == "__main__":
    unittest.main()
