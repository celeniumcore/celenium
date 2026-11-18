# Uji crash/restart faucet lewat HTTP dan treasury

## Perintah

Jalankan dari root repo. Linux, Python 3, dan `cc` diperlukan. Tidak ada dependency Python tambahan.

```sh
cargo build --offline -j 4 -p celenium-stratum-bridge --example recovery_probe
python3 scripts/test-faucet-recovery.py --self-test
python3 scripts/test-faucet-recovery.py
```

Simpan seluruh ledger, journal, marker, dan log proses dalam direktori baru:

```sh
python3 scripts/test-faucet-recovery.py \
  --work-dir "$TMPDIR/faucet-recovery-evidence"
```

`--probe /path/to/recovery_probe` memilih binary yang sudah dibangun. Skrip tidak
menjalankan Cargo. Uji kompatibilitas journal di crate:

```sh
cargo test --offline -j 4 -p celenium-stratum-bridge --lib treasury::tests
```

Gate tambahan untuk pembacaan body HTTP yang terfragmentasi:

```sh
python3 scripts/test-faucet-recovery.py --check-http-framing
```

## Apa yang diuji

`recovery_probe node` menjalankan node simnet terisolasi dengan genesis reserve
eksperimental, storage-mass standar, RPC/P2P loopback, DNS/UPnP nonaktif, serta
peer masuk/keluar nol. Semua wallet dibuat acak khusus tes. Tidak ada kunci dari
environment faucet, data produksi, worker stratum, atau koneksi jaringan publik.
Simnet melewati proof-of-work; transaksi tetap melalui validasi node.

`recovery_probe bridge` memuat ledger relatif cwd seperti startup bridge,
memasang `CeleniumApi`, state, dan reserve signer pada `prom`, memulai
`treasury::start`, dan menjalankan server HTTP produksi. Probe tidak memanggil
settlement atau menghapus journal sendiri. Proses node bertahan selama proses
bridge dimatikan dan dibuat ulang. Ini menguji komponen bridge sebenarnya di
example child, bukan startup CLI lengkap `stratum-bridge`.

Setiap skenario memakai cwd ledger/journal sendiri dan alamat penerima berbeda.
Wallet operasional kosong; payout memakai reserve genesis simnet. HTTP claim
memberi peserta lain reward pool dinamis. Withdrawal sebesar reward pool harus menghasilkan
`SmallTransferNotSupported` sampai saldo terkumpul mencapai minimum standar,
dan mempertahankan saldo ledger serta journal kosong.
Payout yang berhasil memakai **kewajiban ledger lama 1 CEN**. Fixture ini tidak
mengubah reward harian dan tidak membuktikan payout pool dinamis di mainnet.

Skrip mengompilasi interposer C ke sandbox, lalu memberi `LD_PRELOAD` hanya pada
proses bridge yang akan dimatikan. Tidak ada fault hook baru di source runtime
produksi. Interposer menulis marker, menjalankan `SIGSTOP`, lalu harness mengirim
`SIGKILL` dan memverifikasi exit code `-9` sebelum membuat proses baru.

| Batas crash | Lokasi yang ditahan | Kondisi ledger di disk |
| --- | --- | --- |
| `before-broadcast` | Sesudah rename dan fsync direktori journal pending, sebelum submit RPC | Utang 1 CEN masih ada |
| `after-broadcast` | Sebelum rename ledger setelah submit RPC mengembalikan txid | Utang 1 CEN masih ada |
| `after-ledger-save` | Sesudah rename dan fsync direktori ledger | Utang sudah nol, txid settlement tersimpan |
| `before-clear` | Sebelum rename journal yang menghapus pending oleh treasury setelah konfirmasi | Utang sudah nol, journal disk masih pending |

Setelah restart, handler HTTP menahan withdrawal kedua selama pending. Treasury
harus memuat dan mengirim transaksi bertanda tangan yang sama, memeriksa
acceptance history, menyimpan ledger secara idempotent, dan mengosongkan journal.
Skrip memeriksa txid/signed JSON tetap sama, debit/fee satu kali, dan reserve turun
persis payout + fee. Wallet operasional harus tetap nol. Restart tambahan sesudah
recovery harus mempertahankan settlement dan menolak pembayaran kedua.

Pada `before-clear`, penerima membelanjakan output saat bridge mati. Restart harus
menyelesaikan rekonsiliasi walaupun output awal sudah tidak ada di indeks UTXO.

Fixture ledger lama menghilangkan field opsional baru dan berisi saldo di atas
cap 500.000 CEN. HTTP balance harus mempertahankan nilai lama dan claim baru harus
ditolak `ReservoirFull`. Journal lama yang kehilangan `withdrawal`, `anchor`, dan
`input_entries` harus tetap dapat dimuat. Jika validasi pembayaran tidak lengkap,
treasury harus fail closed: pending tidak berubah dan withdrawal baru ditahan.
Tes ini tidak menganggap metadata yang hilang dapat direkonstruksi secara aman.

## Hasil eksekusi lokal

Probe baru berhasil dikompilasi langsung dengan `rustc` dan dependency cache yang
konsisten dari fingerprint example `mining_reserve_probe`. Cargo tidak dijalankan
pada sesi implementasi ini. Tiga tes journal/migrasi dari source `treasury.rs` juga
lulus melalui harness `rustc --test`: journal lama, metadata pending lama, dan
save/reload saldo di atas cap dengan settlement idempotent.
Parent tetap perlu menjalankan perintah Cargo di atas pada source final.

Suite HTTP + treasury menghasilkan empat hasil crash sukses dan
`FAUCET_HTTP_CRASH_RECOVERY_PASSED`, termasuk ledger di atas cap dan journal lama
yang fail closed. Fee terukur tiap payout 1 CEN adalah 509 pradha. Bukti sesi
tersimpan di scratch lokal `faucet-recovery-evidence-final`; fixture identitas berisi
kunci simnet sekali pakai, jadi direktori bukti tidak untuk dipublikasikan.
`--self-test` juga lulus pada empat batas interposer dengan proses C nyata.

**Temuan HTTP terpisah masih terbuka:** `--check-http-framing` gagal deterministik
dengan `{"error":"invalid JSON body","errorCode":"InvalidRequest"}`. Pada source
saat ini `prom::handle_one_connection` hanya melakukan satu `read`, sehingga header
dan body POST yang dikirim dalam dua tahap tidak dirangkai berdasarkan
`Content-Length`. Suite recovery utama mengirim header+body dengan satu
`sendall`; keberhasilannya tidak meloloskan gate HTTP framing ini. Perbaikan
berada di `bridge/src/prom.rs`, di luar kepemilikan perubahan probe/treasury.
Tidak ada klaim kesiapan atau deployment mainnet dari hasil lokal ini.
