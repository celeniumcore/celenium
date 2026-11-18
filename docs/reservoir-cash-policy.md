# Reservoir faucet dan kas miner

Kebijakan ekonomi berjalan di bridge. RPC node memvalidasi ulang kandidat indeks
terhadap UTXO konsensus sebelum mengembalikan dana; bridge menuntut penanda
`utxo-consensus-v1`. Perubahan ini tidak mengubah genesis, suplai, atau jadwal
subsidi konsensus. Target awal bukan bukti pendanaan: reservoir baru terisi ketika
reward atau transfer masuk sebagai UTXO yang dapat dibelanjakan.

## Target yang menurun

`floor(m) = 250000000000000 × remaining_emission(m) / remaining_emission(0)`

Target floor mengatur akumulasi dan perlindungan surplus; plafon klaim adalah batas
kewajiban ledger, bukan sub-wallet atau dana yang dipisahkan. Keduanya berbeda:
plafon kewajiban klaim baru tetap 500.000 CEN, sementara klaim hanya dibayar dari
saldo matang reservoir yang benar-benar tersedia. Worker aktif tidak diperlukan
untuk membayar dari saldo yang sudah terkumpul.

Nilai dalam pradha, perkalian `u128`, pembagian dibulatkan ke bawah. Bulan
operasional berasal dari virtual DAA node pada 10 BPS, bukan tanggal komputer.
Floor mulai turun pada bulan pertama dan menjadi nol pada akhir emisi. Fungsi
legacy berbasis timestamp tetap tersedia untuk pemanggil lama; pengamanan dana
pada API faucet dan treasury memakai snapshot node.

## Dana nyata dan hak klaim

- Snapshot dana memakai output yang telah diverifikasi ulang di UTXO konsensus,
  matang pada script alamat reservoir, tanpa covenant. Bridge juga mengeluarkan
  input yang sudah dibelanjakan di mempool.
- Snapshot kedaluwarsa setelah 30 detik. Kegagalan node menghentikan klaim baru.
- Plafon kewajiban klaim baru adalah 500.000 CEN. Angka ini membatasi total saldo klaim off-chain, bukan saldo wallet atau dana yang otomatis tersedia.
- Klaim dibayar dari saldo matang reservoir setelah kewajiban dan buffer biaya 2,5 CEN. Pembayaran dapat berjalan tanpa miner aktif selama dana tersebut mencukupi.
- Floor dinamis tetap menjadi target saldo reservoir dan batas perlindungan saat surplus dikirim ke kas miner. Floor bukan sumber dana terpisah dan tidak melarang pembayaran klaim memakai saldo di reservoir.
- Kewajiban ledger lama di atas plafon tetap utuh; sistem tidak menghapus atau memotong hak yang sudah tercatat.
- Dana bebas = saldo matang dikurangi kewajiban dan buffer biaya 2,5 CEN.
- Anggaran harian = minimum dana bebas dengan maksimum batas emisi legacy,
  dana bebas / 365, dan batas minimum reward pool bila dana bebas mencukupi.
  Anggaran ditetapkan per hari UTC dan disimpan bersama ledger;
  refresh saldo tidak terus menaikkan anggaran untuk hari yang sama.
- Kuota klaim dibagi dari pool harian. Batas awal 100.000–500.000 pradha dibagi
  dua setiap 48 bulan; bila dana atau batas minimum tidak cukup, klaim ditolak
  tanpa kredit parsial.
- Setelah subsidi berhenti, cadangan nyata masih dapat membiayai klaim. Tidak ada
  janji pendanaan abadi: tanpa pendapatan baru, klaim akhirnya berhenti saat dana
  bebas tidak cukup.

Target ini adalah cadangan likuiditas bersama. Batas kewajiban klaim runtime
mengikuti dana nyata; perhitungan bucket subsidi Android legacy tidak mengeksekusi
payout miner dan tidak boleh dilaporkan sebagai dana yang sudah dibayar.

## Bootstrap operator

Konfigurasi publik:

```yaml
faucet_address: "<alamat reservoir>"
faucet_bootstrap_address: "<alamat worker operator>"
faucet_surplus_address: "<alamat kas miner>"
```

Hanya worker dengan alamat yang sama persis dengan `faucet_bootstrap_address`
yang dialihkan coinbase-nya ke reservoir ketika saldo matang di bawah floor.
Saat floor tercapai, template kembali memakai alamat pool atau worker normal.
Tanpa snapshot segar, bridge tidak mengalihkan reward. Worker lain tidak ikut
mendanai bootstrap. VPS tetap node-only; operator menghubungkan mesin eksternal
ke stratum dengan alamat bootstrap sebagai identitas worker.

Bootstrap tidak mencetak atau memindahkan CEN yang belum ditambang. Kas miner
adalah wallet biasa; dana yang kembali tidak menjadi subsidi blok baru.

## Surplus dan cadangan akhir emisi

Dana terlindungi = maksimum floor dengan jumlah berikut:

- hak klaim ledger yang belum dibayar;
- buffer biaya operasional 2,5 CEN;
- cadangan layanan 365 × maksimum pengeluaran harian yang tercatat, setidaknya
  100.000 pradha per hari selama era reward awal.

Puncak pengeluaran disimpan dalam ledger agar pergantian hari atau restart tidak
langsung mengurangi cadangan. Biaya transaksi surplus dicadangkan sebelum kirim.
Sweep hanya bila nominalnya setidaknya 1 CEN. Transaksi dibatasi kapasitas input
batch yang valid; sisa surplus diproses pada batch berikutnya, dengan jeda 60 detik
setelah batch sebelumnya selesai. Pembulatan dan biaya tidak boleh menembus dana
terlindungi.

`reservoir_transfers.json` menyimpan transaksi bertanda tangan sebelum broadcast.
Transaksi pending menahan pengiriman baru. Setelah restart, bridge mengirim ulang
byte transaksi yang sama dan memeriksa penerimaan dengan sedikitnya 10 konfirmasi
dari anchor chain sebelum broadcast; ia tidak membentuk transaksi pengganti dari
input lain. Penarikan klaim memakai jurnal yang sama dan
ID transaksi settlement disimpan untuk mencegah debit ledger dua kali.

Posisi halaman riwayat diperbarui di jurnal agar pemeriksaan yang melewati batas
batch RPC dapat dilanjutkan setelah restart. Metadata alamat, jumlah, biaya,
network, dan entri input diverifikasi terhadap transaksi bertanda tangan sebelum
rekonsiliasi maupun broadcast.

Anchor yang sudah terpangkas sebelum rekonsiliasi atau penolakan transaksi
permanen memerlukan investigasi operator. Bridge memilih menahan transfer dan
kredit baru, bukan menganggapnya gagal lalu membayar ulang. Jangan menghapus
jurnal pending tanpa memeriksa transaksi dan kewajiban ledger.

## Kunci dan status

Signer reservoir tetap dibaca dari `CELENIUM_FAUCET_KEYFILE` atau environment yang
sudah dikonfigurasi. Kunci kas miner terpisah dan bukan kunci signer reservoir.
Kunci tidak disimpan di repo atau config YAML.

`GET /bridge/api/faucet/status` mengekspos model floor, fase emisi node, saldo
matang, dana bebas, kewajiban, buffer, cadangan layanan, alamat bootstrap/kas miner,
serta ID transaksi pending/terakhir. Status terkonfigurasi tidak membuktikan
reservoir awal sudah terisi.

## Verifikasi

```sh
rustc --edition=2021 --test bridge/src/emission_policy.rs -o "$TMPDIR/floor-tests"
"$TMPDIR/floor-tests"
rustc --edition=2021 --test bridge/src/reservoir_funds.rs -o "$TMPDIR/funds-tests"
"$TMPDIR/funds-tests"
cargo test --release -p celenium-stratum-bridge
cargo run --release -p celenium-stratum-bridge --example reservoir_transfer_probe
```

Probe memakai simnet lokal, parameter konsensus default, tanpa peer eksternal,
serta kunci sementara di memori. Simnet melewati PoW; validasi transaksi tetap
berjalan. Uji ini bukan bukti bahwa mainnet telah didanai.
