# Faucet: akrual 24 jam

Kontrak perubahan bridge. Klaim mencatat saldo partisipasi off-chain. Penarikan ke wallet on-chain tetap melalui `/api/faucet/withdraw`, dengan signer, dana reservoir, minimum, cooldown, dan anggaran fee yang berlaku.

## Akrual dan klaim

- Hari mengikuti UTC. Kuota per alamat tetap dinamis, dengan plafon 1.000 pradha per hari. Anggaran emisi dan batas kewajiban ledger tetap berlaku.
- Kuota disimpan pada klaim pertama yang berhasil. Dalam hari itu, jumlah terakru adalah `floor(kuota × detik_sejak_00:00 / 86400)`.
- Klaim membayar selisih antara akrual dan jumlah yang sudah diklaim. Klaim ulang pada waktu yang sama tidak membayar lagi.
- Sisa kuota dari hari yang sudah dibuka menjadi matang saat hari berganti dan dapat diambil pada klaim berikutnya. Tidak ada pemberian kuota untuk setiap hari yang dilewati tanpa klaim.
- Pembayaran sisa hari sebelumnya memakai anggaran pada hari pembayaran. Jika anggaran atau kapasitas ledger tidak cukup, klaim ditolak dan saldo tetap utuh.
- Saldo lama tetap utuh. Entri legacy yang sudah klaim hari ini dianggap selesai untuk hari itu; upgrade tidak menerbitkan klaim kedua.
- Jumlah peserta menghitung alamat unik yang membuka kuota hari itu, bukan banyaknya panggilan klaim.

Contoh tes, bukan saldo pengguna: pada kuota 1.000 pradha, akrual pukul 00:30 UTC adalah 20 pradha. Pada 01:00 UTC totalnya 41 pradha; jika 20 sudah diklaim, klaim berikutnya membayar 21.

## API

`GET /api/faucet/balance?address=...`:

- `balancePradha`, `totalClaimedPradha`: saldo ledger dan total historis.
- `dailyRewardPradha`: kuota hari ini.
- `accruedTodayPradha`, `claimedTodayPradha`: akrual dan jumlah yang sudah dikreditkan hari ini.
- `pendingPreviousPradha`: sisa kuota yang sudah matang dari hari aktif sebelumnya.
- `rewardPradha`: jumlah yang dapat diklaim sekarang setelah pemeriksaan batas.
- `canClaimToday`: benar bila `rewardPradha > 0`.
- `canStartDrip`: aktivasi diperlukan saat kuota hanya 1 pradha dan belum dibuka. POST pertama mengaktifkan kuota tanpa kredit (`startedDrip: true`, `rewardPradha: 0`); satu pradha matang saat hari berganti.
- `serverTimeSecs`, `dripDurationSecs`, `dripModel`: waktu server, 86400, `linear-utc-day`.

`POST /api/faucet/claim` tetap menerima `address` dan `deviceId`. Respons sukses `rewardPradha` adalah jumlah yang benar-benar dikreditkan pada panggilan tersebut, bukan total harian. Galat menyediakan `error` dan `errorCode`.

Waktu klaim hanya berasal dari server. Parameter waktu pengujian tidak diekspos melalui HTTP.

## Persistensi dan deployment

Klaim baru harus tersimpan sebelum respons sukses. File ledger ditulis melalui berkas sementara dan rename; JSON rusak menyebabkan startup gagal, bukan mengosongkan saldo. Backup ledger dan binary harus dibuat sebelum mengganti bridge.

Tes lokal memakai ledger terisolasi dan waktu terkendali. Probe produksi bersifat baca-saja; verifikasi API tidak membuktikan pembayaran on-chain.
