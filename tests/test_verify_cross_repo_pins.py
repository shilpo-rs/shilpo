#!/usr/bin/env python3

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
VERIFIER = REPO_ROOT / "scripts" / "verify_cross_repo_pins.py"


def run(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        check=True,
        text=True,
        capture_output=True,
    )


def create_remote(root: Path, owner: str, name: str) -> tuple[Path, str]:
    work = root / "work" / name
    remote = root / "remotes" / owner / f"{name}.git"
    work.mkdir(parents=True)
    remote.parent.mkdir(parents=True)
    run("git", "init", "--initial-branch=main", cwd=work)
    run("git", "config", "user.name", "Pin Fixture", cwd=work)
    run("git", "config", "user.email", "pin-fixture@example.invalid", cwd=work)
    (work / "README.md").write_text("fixture\n", encoding="utf-8")
    run("git", "add", "README.md", cwd=work)
    run("git", "commit", "-m", "fixture revision", cwd=work)
    revision = run("git", "rev-parse", "HEAD", cwd=work).stdout.strip()
    run("git", "clone", "--bare", str(work), str(remote))
    return remote, revision


def snapshot(root: Path) -> dict[str, bytes]:
    return {
        str(path.relative_to(root)): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


class VerifyCrossRepoPinsTests(unittest.TestCase):
    def test_verifies_cargo_revision_and_does_not_mutate_consumer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            remote, revision = create_remote(root, "acme", "widget")
            consumer = root / "consumer"
            consumer.mkdir()
            (consumer / "Cargo.toml").write_text(
                "[package]\n"
                'name = "consumer"\n'
                'version = "0.1.0"\n'
                "\n[dependencies]\n"
                f'widget = {{ git = "{remote}", rev = "{revision}" }}\n',
                encoding="utf-8",
            )
            before = snapshot(consumer)

            result = subprocess.run(
                [
                    sys.executable,
                    str(VERIFIER),
                    "--repo",
                    f"example/consumer={consumer}",
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("consumer=example/consumer", result.stdout)
            self.assertIn("source=acme/widget", result.stdout)
            self.assertIn(f"revision={revision}", result.stdout)
            self.assertEqual(snapshot(consumer), before)

    def test_reports_owning_repository_when_revision_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            remote, _ = create_remote(root, "acme", "contracts")
            missing_revision = "f" * 40
            consumer = root / "sdk"
            consumer.mkdir()
            (consumer / "Cargo.toml").write_text(
                "[package]\n"
                'name = "sdk"\n'
                'version = "0.1.0"\n'
                "\n[build-dependencies]\n"
                f'contracts = {{ git = "{remote}", rev = "{missing_revision}" }}\n',
                encoding="utf-8",
            )
            before = snapshot(consumer)

            result = subprocess.run(
                [
                    sys.executable,
                    str(VERIFIER),
                    "--repo",
                    f"example/sdk={consumer}",
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 1)
            self.assertIn("MISSING consumer=example/sdk", result.stdout)
            self.assertIn("source=acme/contracts", result.stdout)
            self.assertIn(f"revision={missing_revision}", result.stdout)
            self.assertIn("Failed to resolve 1 of 1", result.stderr)
            self.assertEqual(snapshot(consumer), before)

    def test_verifies_sdk_wit_revision_against_contract_repository(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            remote, revision = create_remote(root, "shilpo-rs", "shilpo")
            sdk = root / "sdks"
            sdk.mkdir()
            (sdk / "WIT_REV").write_text(f"{revision}\n", encoding="utf-8")

            result = subprocess.run(
                [
                    sys.executable,
                    str(VERIFIER),
                    "--repo",
                    f"shilpo-rs/sdks={sdk}",
                    "--wit-source",
                    str(remote),
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("consumer=shilpo-rs/sdks", result.stdout)
            self.assertIn("source=shilpo-rs/shilpo", result.stdout)
            self.assertIn(f"revision={revision}", result.stdout)
            self.assertIn("location=WIT_REV", result.stdout)

    def test_expands_short_cargo_revision_from_lockfile_before_fetching(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            remote, revision = create_remote(root, "acme", "large-repo")
            short_revision = revision[:7]
            consumer = root / "consumer"
            consumer.mkdir()
            (consumer / "Cargo.toml").write_text(
                "[package]\n"
                'name = "consumer"\n'
                'version = "0.1.0"\n'
                "\n[dependencies]\n"
                f'large = {{ git = "{remote}", rev = "{short_revision}" }}\n',
                encoding="utf-8",
            )
            (consumer / "Cargo.lock").write_text(
                "version = 4\n\n"
                "[[package]]\n"
                'name = "large"\n'
                'version = "0.1.0"\n'
                f'source = "git+{remote}?rev={short_revision}#{revision}"\n',
                encoding="utf-8",
            )

            result = subprocess.run(
                [
                    sys.executable,
                    str(VERIFIER),
                    "--repo",
                    f"example/consumer={consumer}",
                ],
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"revision={short_revision}", result.stdout)
            self.assertIn(f"resolved={revision}", result.stdout)


if __name__ == "__main__":
    unittest.main()
