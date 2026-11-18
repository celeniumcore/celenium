# Faucet: akrual 24 jam

Kontrak perubahan bridge. Klaim mencatat saldo partisipasi off-chain. Penarikan ke wallet on-chain tetap melalui `/api/faucet/withdraw`, dengan signer, dana reservoir, minimum, cooldown, dan anggaran fee yang berlaku.

## Akrual dan klaim

- Hari mengikuti UTC. Pool harian dibagi menurut saldo matang dan jumlah pengklaim hari sebelumnya. Reward awal berada di rentang 100.000 sampai 500.000 pradha per alamat.
- Batas minimum dan maksimum dibagi dua setiap 48 bulan konsensus. Jika dana pool tidak cukup untuk batas minimum, sistem menolak kredit baru tanpa kredit parsial.
- Kuota harian disimpan pada refresh funding pertama hari itu. Restart, refresh saldo, atau peserta baru tidak mengubah kuota yang sudah dibuka.
- Klaim membayar kuota yang sudah dibekukan. Klaim ulang pada hari yang sama tidak membayar lagi.
- Sisa kuota dari hari yang sudah dibuka menjadi matang saat hari berganti dan dapat diambil pada klaim berikutnya. Tidak ada pemberian kuota untuk setiap hari yang dilewati tanpa klaim.
- Pembayaran sisa hari sebelumnya memakai anggaran pada hari pembayaran. Jika anggaran atau kapasitas ledger tidak cukup, klaim ditolak dan saldo tetap utuh.
- Saldo lama tetap utuh. Entri legacy yang sudah klaim hari ini dianggap selesai untuk hari itu; upgrade tidak menerbitkan klaim kedua.
- Jumlah peserta menghitung alamat unik yang membuka kuota hari itu, bukan banyaknya panggilan klaim.

Minimum penarikan mengikuti respons server dan saat ini 2.500.000 pradha. Saldo
reward tetap off-chain sampai ambang itu tercapai, agar transaksi kecil tidak
ditolak storage mass standar.

## API

`GET /api/faucet/balance?address=...`:

- `balancePradha`, `totalClaimedPradha`: saldo ledger dan total historis.
- `dailyRewardPradha`: kuota hari ini.
- `accruedTodayPradha`, `claimedTodayPradha`: akrual dan jumlah yang sudah dikreditkan hari ini.
- `pendingPreviousPradha`: sisa kuota yang sudah matang dari hari aktif sebelumnya.
- `rewardPradha`: jumlah yang dapat diklaim sekarang setelah pemeriksaan batas.
- `canClaimToday`: benar bila `rewardPradha > 0`.
- `minRewardPradha`, `maxRewardPradha`: batas reward pada bulan emisi aktif.
- `minWithdrawPradha`, `halvingIntervalMonths`, `rewardEmissionMonth`: ambang tarik dan konteks jadwal reward dari server.
- `serverTimeSecs`, `dripDurationSecs`, `dripModel`: waktu server, 0, `none`.

`POST /api/faucet/claim` tetap menerima `address` dan `deviceId`. Respons sukses `rewardPradha` adalah jumlah yang benar-benar dikreditkan pada panggilan tersebut, bukan total harian. Galat menyediakan `error` dan `errorCode`.

Waktu klaim hanya berasal dari server. Parameter waktu pengujian tidak diekspos melalui HTTP.

## Persistensi dan deployment

Klaim baru harus tersimpan sebelum respons sukses. File ledger ditulis melalui berkas sementara dan rename; JSON rusak menyebabkan startup gagal, bukan mengosongkan saldo. Jika sinkronisasi direktori gagal setelah rename, state terpasang dipertahankan, API mengembalikan `StorageUnavailable`, dan klaim serta penarikan ditahan sampai storage dipulihkan. Backup ledger dan binary harus dibuat sebelum mengganti bridge.

Tes lokal memakai ledger terisolasi dan waktu terkendali. Probe produksi bersifat baca-saja; verifikasi API tidak membuktikan pembayaran on-chain.
