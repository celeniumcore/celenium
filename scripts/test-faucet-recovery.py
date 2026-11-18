#!/usr/bin/env python3
"""Kill isolated bridge processes at durable boundaries and inspect automatic replay."""
import argparse
import json
import os
from pathlib import Path
import queue
import signal
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.parse

AMOUNT = 100_000_000
CAP = 50_000_000_000_000
BOUNDARIES = ("before-broadcast", "after-broadcast", "after-ledger-save", "before-clear")

# This shim is compiled into the sandbox and loaded only in disposable children.
INTERPOSER = r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static __thread int journal_saved, ledger_saved;
static int named(const char *path, const char *name) {
    const char *base = strrchr(path, '/');
    return strcmp(base ? base + 1 : path, name) == 0;
}
static void hit(const char *point) {
    const char *selected = getenv("RECOVERY_CRASH_POINT");
    const char *marker = getenv("RECOVERY_CRASH_MARKER");
    if (!selected || !marker || strcmp(point, selected)) return;
    int fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) _exit(91);
    write(fd, point, strlen(point));
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    real_sync(fd);
    close(fd);
    raise(SIGSTOP);
    _exit(92);
}
int rename(const char *from, const char *to) {
    int (*real_rename)(const char *, const char *) = dlsym(RTLD_NEXT, "rename");
    if (named(to, "faucet_ledger.json")) hit("after-broadcast");
    if (named(to, "reservoir_transfers.json")) {
        int fd = open(from, O_RDONLY);
        char bytes[256] = {0};
        if (fd >= 0) { read(fd, bytes, sizeof(bytes)-1); close(fd); }
        if (strstr(bytes, "\"pending\": null")) hit("before-clear");
    }
    int result = real_rename(from, to);
    if (!result) {
        journal_saved = named(to, "reservoir_transfers.json");
        ledger_saved = named(to, "faucet_ledger.json");
    }
    return result;
}
int fsync(int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    int result = real_sync(fd);
    if (!result && journal_saved) { journal_saved = 0; hit("before-broadcast"); }
    if (!result && ledger_saved) { ledger_saved = 0; hit("after-ledger-save"); }
    return result;
}
'''


def read_json(path):
    return json.loads(Path(path).read_text())


def wait_for(check, message, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(message)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def build_interposer(directory):
    source = directory / "crash.c"
    library = directory / "crash.so"
    source.write_text(INTERPOSER)
    subprocess.run(["cc", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-o", str(library), str(source), "-ldl"], check=True)
    return library


class Child:
    def __init__(self, command, cwd, env=None):
        self.events = queue.Queue()
        self.log = (cwd / ("child-" + str(time.monotonic_ns()) + ".log")).open("w")
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.process.stdout:
            self.log.write(line)
            self.log.flush()
            if line.startswith("RECOVERY "):
                self.events.put(json.loads(line.removeprefix("RECOVERY ")))

    def event(self, timeout=45):
        try:
            return self.events.get(timeout=timeout)
        except queue.Empty as error:
            raise AssertionError(f"probe timed out; exit={self.process.poll()} log={self.log.name}") from error

    def command(self, **value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()
        return self.event()

    def kill(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGKILL)
        code = self.process.wait(timeout=10)
        self.reader.join(timeout=5)
        self.log.close()
        return code


def http(bind, path, value=None, split_body=False):
    import http.client
    body = b"" if value is None else json.dumps(value).encode()
    method = "GET" if value is None else "POST"
    header = (f"{method} {path} HTTP/1.1\r\nHost: {bind}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n").encode()
    host, port = bind.split(":")
    with socket.create_connection((host, int(port)), timeout=20) as connection:
        if split_body:
            connection.sendall(header)
            time.sleep(0.1)
            connection.sendall(body)
        else:
            connection.sendall(header + body)
        response = http.client.HTTPResponse(connection)
        response.begin()
        assert response.status == 200
        assert "application/json" in response.headers["Content-Type"]
        return json.load(response)


def ready(bind):
    try:
        return http(bind, "/api/faucet/status")
    except (OSError, ValueError):
        return None


def clean_env():
    return {key: value for key, value in os.environ.items() if not key.startswith(("FAUCET_", "RECOVERY_")) and key != "LD_PRELOAD"}


def legacy_ledger(recipient, amount=AMOUNT):
    # Deliberately omit newer optional fields to test deserialization defaults.
    return {"ledger": {recipient: {"balance_pradha": amount, "last_claim_day": 0, "total_claimed_pradha": amount}}, "devices": {}, "claimed_today": 0, "claim_day": 0}


def check_ledger(path, recipient, expected, txid=None, fee=None):
    ledger = read_json(path)
    assert ledger["ledger"][recipient]["balance_pradha"] == expected, ledger
    if txid is not None:
        assert ledger["settled_withdrawal_txids"].count(txid) == 1
        assert ledger["fees_today_pradha"] == fee
    return ledger


def run(probe, directory, check_http_framing=False):
    library = build_interposer(directory)
    env = clean_env()
    node = Child([str(probe), "node", str(directory)], directory, env)
    bridge = None
    first_pending = None
    try:
        info = node.event()
        assert info["ready"] and info["defaultStorageMass"]
        rpc = info["rpc"]
        for index, boundary in enumerate(BOUNDARIES):
            work = directory / boundary
            work.mkdir()
            (work / ".recovery-sandbox").touch()
            recipient, participant = info["recipients"][index], info["participants"][index]
            starting_reserve = node.command(command="check", index=index)["reserve"]
            ledger_path, journal_path = work / "faucet_ledger.json", work / "reservoir_transfers.json"
            ledger_path.write_text(json.dumps(legacy_ledger(recipient)))
            # Old empty journals did not contain withdrawal/anchor/input metadata.
            journal_path.write_text('{"pending":null,"last_txid":null,"total_returned_pradha":0,"last_sweep_secs":0}')
            bind = f"127.0.0.1:{free_port()}"
            args = [str(probe), "bridge", str(directory), rpc, bind]
            bridge = Child(args, work, env)
            wait_for(lambda: ready(bind), "HTTP did not start")
            claim = http(bind, "/api/faucet/claim", {"address": participant, "deviceId": f"recovery-{index}"}, split_body=check_http_framing)
            assert claim.get("ok") and 100_000 <= claim["rewardPradha"] <= 500_000, claim
            if check_http_framing:
                print("HTTP_FRAGMENTED_BODY_PASSED", flush=True)
                return
            small = http(bind, "/api/faucet/withdraw", {"address": participant})
            assert small.get("minAmountPradha") == 2_500_000, small
            assert "minimum withdrawal" in small.get("error", ""), small
            check_ledger(ledger_path, participant, claim["rewardPradha"])
            assert read_json(journal_path)["pending"] is None
            bridge.kill()
            bridge = None

            marker = work / "crash.marker"
            hooked = dict(env, LD_PRELOAD=str(library), RECOVERY_CRASH_POINT=boundary, RECOVERY_CRASH_MARKER=str(marker))
            bridge = Child(args, work, hooked)
            wait_for(lambda: ready(bind), "restart HTTP did not start")
            outcome = []
            def withdraw():
                try:
                    outcome.append(http(bind, "/api/faucet/withdraw", {"address": recipient, "amountPradha": AMOUNT}))
                except (OSError, ValueError) as error:
                    outcome.append(type(error).__name__)
            request = threading.Thread(target=withdraw)
            request.start()
            if boundary == "before-clear":
                request.join(timeout=25)
                assert outcome and isinstance(outcome[0], dict) and outcome[0].get("ok"), outcome
                node.command(command="mine", count=15)
            wait_for(lambda: marker.exists(), f"crash boundary was not reached: {boundary}")
            assert marker.read_text() == boundary
            pending = read_json(journal_path)["pending"]
            assert pending["source"] == info["reserve"] and pending["destination"] == recipient
            assert pending["withdrawal"] and pending["amount_pradha"] == AMOUNT
            txid, fee, signed = pending["transaction_id"], pending["fee_pradha"], pending["transaction_json"]
            if index == 0:
                first_pending = dict(pending)
            assert bridge.kill() == -signal.SIGKILL
            bridge = None
            request.join(timeout=25)
            assert not request.is_alive()
            if boundary != "before-clear":
                assert not any(isinstance(result, dict) and result.get("ok") for result in outcome)
            expected = 0 if boundary in ("after-ledger-save", "before-clear") else AMOUNT
            check_ledger(ledger_path, recipient, expected)
            if expected == 0:
                check_ledger(ledger_path, recipient, 0, txid, fee)

            before = node.command(command="check", index=index, txid=txid)
            assert before["balance"] == (AMOUNT if boundary == "before-clear" else 0) and before["operational"] == 0
            if boundary == "before-clear":
                # Spend while bridge is dead; recovery must use acceptance history.
                node.command(command="spend", index=index)
                node.command(command="mine", count=15)
                assert not node.command(command="check", index=index, txid=txid)["outputPresent"]
            bridge = Child(args, work, env)
            wait_for(lambda: ready(bind), "recovery HTTP did not start")
            blocked = http(bind, "/api/faucet/withdraw", {"address": recipient})
            if boundary == "before-clear":
                assert "error" in blocked, blocked
            else:
                assert blocked.get("errorCode") == "TransferPending", blocked
            duplicate_claim = http(bind, "/api/faucet/claim", {"address": participant, "deviceId": f"recovery-{index}"})
            if "error" not in duplicate_claim:
                diagnostic = http(bind, "/api/faucet/balance?address=" + urllib.parse.quote(participant))
                status = http(bind, "/api/faucet/status")
                raise AssertionError({"duplicateClaim": duplicate_claim, "balance": diagnostic, "status": status})
            if boundary != "before-clear":
                replayed = read_json(journal_path)["pending"]
                assert replayed["transaction_json"] == signed and replayed["transaction_id"] == txid
                wait_for(lambda: http(bind, "/api/faucet/status")["treasury"]["pendingTxid"] == txid, "treasury did not observe pending journal")
                node.command(command="mine", count=15)
                received = wait_for(lambda: (data if (data := node.command(command="check", index=index, txid=txid))["balance"] == AMOUNT else None), "payment missing")
                assert received["outputPresent"]
            wait_for(lambda: read_json(journal_path)["pending"] is None, "treasury did not clear confirmed journal", timeout=100)
            check_ledger(ledger_path, recipient, 0, txid, fee)
            ending = node.command(command="check", index=index, txid=txid)
            assert ending["reserve"] == starting_reserve - AMOUNT - fee, ending
            assert ending["operational"] == 0
            assert read_json(journal_path)["last_txid"] == txid
            assert read_json(journal_path)["total_returned_pradha"] == 0
            balance = http(bind, "/api/faucet/balance?address=" + urllib.parse.quote(recipient))
            assert balance["balancePradha"] == 0, balance
            bridge.kill()
            bridge = Child(args, work, env)
            wait_for(lambda: ready(bind), "second recovery restart failed")
            check_ledger(ledger_path, recipient, 0, txid, fee)
            assert "error" in http(bind, "/api/faucet/withdraw", {"address": recipient})
            bridge.kill()
            bridge = None
            print(json.dumps({"boundary": boundary, "sigkill": True, "txid": txid, "feePradha": fee, "legacyLiabilityPradha": AMOUNT, "ledgerDebitOnce": True, "automaticTreasuryReplay": True}), flush=True)

        work = directory / "legacy-over-cap"
        work.mkdir()
        (work / ".recovery-sandbox").touch()
        recipient = info["recipients"][0]
        (work / "faucet_ledger.json").write_text(json.dumps(legacy_ledger(recipient, CAP + AMOUNT)))
        bind = f"127.0.0.1:{free_port()}"
        bridge = Child([str(probe), "bridge", str(directory), rpc, bind], work, env)
        wait_for(lambda: ready(bind), "legacy migration HTTP failed")
        balance = http(bind, "/api/faucet/balance?address=" + urllib.parse.quote(recipient))
        assert balance["balancePradha"] == CAP + AMOUNT
        rejected = http(bind, "/api/faucet/claim", {"address": info["participants"][0], "deviceId": "over-cap"})
        assert rejected.get("errorCode") == "ReservoirFull", rejected
        check_ledger(work / "faucet_ledger.json", recipient, CAP + AMOUNT)
        bridge.kill()
        bridge = None
        assert first_pending is not None
        legacy_pending = dict(first_pending)
        for field in ("withdrawal", "anchor", "input_entries"):
            legacy_pending.pop(field)
        legacy_journal = {"pending": legacy_pending, "last_txid": None, "total_returned_pradha": 0, "last_sweep_secs": 0}
        journal_path = work / "reservoir_transfers.json"
        journal_path.write_text(json.dumps(legacy_journal))
        bridge = Child([str(probe), "bridge", str(directory), rpc, bind], work, env)
        wait_for(lambda: ready(bind), "legacy pending-journal startup failed")
        failure = wait_for(lambda: http(bind, "/api/faucet/status")["treasury"]["error"], "legacy journal did not fail closed")
        assert failure
        assert read_json(journal_path) == legacy_journal
        check_ledger(work / "faucet_ledger.json", recipient, CAP + AMOUNT)
        assert http(bind, "/api/faucet/withdraw", {"address": recipient})["errorCode"] == "TransferPending"
        print("LEGACY_OVER_CAP_PRESERVED legacy_pending_fail_closed=true", flush=True)
        print("FAUCET_HTTP_CRASH_RECOVERY_PASSED", flush=True)
    finally:
        if bridge is not None:
            bridge.kill()
        node.kill()


class InterposerTests(unittest.TestCase):
    def test_sigkill_at_each_real_persistence_boundary(self):
        root = Path(os.environ.get("TMPDIR", str(Path.home() / ".hermes/profiles/devops/cache/scratch")))
        root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="recovery-shim-", dir=root) as temp:
            directory = Path(temp)
            library = build_interposer(directory)
            source = directory / "writer.c"
            source.write_text(r'''
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
  int fd = open("pending.tmp", O_WRONLY|O_CREAT, 0600);
  write(fd, "{\"pending\": {}}", 15); fsync(fd); close(fd);
  rename("pending.tmp", "reservoir_transfers.json");
  fd = open(".", O_RDONLY); fsync(fd); close(fd);
  fd = open("ledger.tmp", O_WRONLY|O_CREAT, 0600);
  write(fd, "{}", 2); fsync(fd); close(fd);
  rename("ledger.tmp", "faucet_ledger.json");
  fd = open(".", O_RDONLY); fsync(fd); close(fd);
  fd = open("clear.tmp", O_WRONLY|O_CREAT, 0600);
  write(fd, "{\"pending\": null}", 17); fsync(fd); close(fd);
  rename("clear.tmp", "reservoir_transfers.json");
  return 0;
}
''')
            writer = directory / "writer"
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-o", str(writer), str(source)], check=True)
            for boundary in BOUNDARIES:
                with self.subTest(boundary=boundary):
                    work = directory / boundary
                    work.mkdir()
                    marker = work / "marker"
                    process = subprocess.Popen([str(writer)], cwd=work, env=dict(clean_env(), LD_PRELOAD=str(library), RECOVERY_CRASH_POINT=boundary, RECOVERY_CRASH_MARKER=str(marker)))
                    try:
                        wait_for(lambda: marker.exists(), "interposer did not stop child", timeout=5)
                        assert marker.read_text() == boundary
                        process.kill()
                        self.assertEqual(process.wait(timeout=5), -signal.SIGKILL)
                        self.assertTrue((work / "reservoir_transfers.json").is_file())
                        self.assertEqual((work / "faucet_ledger.json").exists(), boundary in ("after-ledger-save", "before-clear"))
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/examples/recovery_probe")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--check-http-framing", action="store_true", help="send POST header/body separately; regression gate for partial TCP reads")
    parser.add_argument("--work-dir", type=Path, help="new directory, retained for evidence")
    args = parser.parse_args()
    if args.self_test:
        results = unittest.main(argv=[__file__], exit=False, verbosity=2).result
        raise SystemExit(0 if results.wasSuccessful() else 1)
    probe = args.probe.resolve()
    if not probe.is_file():
        parser.error(f"probe binary missing: {probe}; build recovery_probe first")
    if args.work_dir:
        directory = args.work_dir.resolve()
        directory.mkdir(mode=0o700, parents=True, exist_ok=False)
        (directory / ".recovery-sandbox").touch()
        print(f"EVIDENCE {directory}", flush=True)
        run(probe, directory, args.check_http_framing)
    else:
        scratch = Path(os.environ.get("TMPDIR", str(Path.home() / ".hermes/profiles/devops/cache/scratch")))
        with tempfile.TemporaryDirectory(prefix="faucet-recovery-", dir=scratch) as temp:
            directory = Path(temp)
            (directory / ".recovery-sandbox").touch()
            run(probe, directory, args.check_http_framing)


if __name__ == "__main__":
    main()
