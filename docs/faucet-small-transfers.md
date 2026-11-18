# Klaim pool dan transaksi kecil

Perubahan setelah rilis drip `06a9ad1`. Klaim dan pembentuk transaksi diuji pada source, serta transfer kecil dibuktikan pada simnet dengan override eksplisit. Aturan transaksi kecil belum aktif di mainnet. Tes gabungan consensus-core dan bridge: 285 lulus; suite aplikasi snapshot commit: 194 lulus. Tes modul faucet/emission_policy terpisah: 52 lulus.

## Permintaan dan perilaku

- Tombol klaim tetap dapat ditekan untuk meminta klaim atau melihat alasan server. Hanya permintaan yang sedang diproses yang menahan tap ganda.
- Reward pool berada pada rentang 100.000–500.000 pradha pada era awal dan dibagi dua setiap 48 bulan. Kuota dibekukan per hari dari dana matang dan peserta hari sebelumnya.
- Klaim ulang hari yang sama tidak mengkredit lagi. Batas perangkat, anggaran harian, kewajiban ledger, dan penyimpanan tetap berlaku.
- Migrasi saldo lama mempertahankan kuota yang sudah tercatat pada hari aktif. Saldo legacy yang sudah dibayar penuh tidak dibayar ulang.
- Penarikan tetap memakai dana reservoir on-chain dan membayar fee dari reservoir.

## Bukti masalah penarikan pada aturan sekarang

Probe simnet menunjukkan payout 100.000 dan 500.000 pradha dari satu UTXO besar masih ditolak storage mass standar, sedangkan payout minimum akumulasi 2.500.000 pradha lolos dengan fee 509 pradha. Output kecil tetap perlu dikumpulkan sebelum penarikan.

Pembentuk transaksi sekarang memilih UTXO berdasarkan saldo, maturity jaringan dan massa signed transaction; tidak membuat change bernilai nol; menghitung fee dari transaksi yang benar-benar dikirim. Fee transaksi pengguna tidak dinaikkan diam-diam. Faucet menyiapkan transaksi terlebih dahulu untuk memeriksa anggaran fee, kemudian mengirim transaksi yang sama.

## Eksperimen protokol transaksi kecil

Perubahan massa storage dapat mengubah validitas transaksi dan blok. Implementasi eksperimen harus opt-in pada jaringan lokal, dengan default semua jaringan tetap sama. Jangan reset genesis, mengganti supply, atau mengaktifkannya sepihak di mainnet. Saldo dan nominal output tetap dalam pradha, tidak dibulatkan untuk membuat transaksi lolos.

Gerbang aktivasi: uji transfer pool 2.500.000 pradha sampai UTXO penerima terkonfirmasi, uji massa/default legacy, batas sumber daya dan spam, parameter konsisten antarnode, mekanisme aktivasi terjadwal, upgrade wallet, backup dan rollback yang kompatibel. Satu node yang menerima transaksi tidak membuktikan peer lama akan menerimanya.

## Bukti pembayaran simnet

Jalankan `cargo run --offline -p celenium-stratum-bridge --example faucet_small_transfer_probe -j 2`.

Node memakai simnet terisolasi, dana dari blok lokal, dan override `storage_mass_amount_floor=4000000`. Tidak ada perubahan nominal output ataupun genesis. Pengujian memakai `FaucetState`, `CeleniumApi::prepare_simnet_transfer` dan `submit_prepared_transfer`; ini bukan uji HTTP atau APK end-to-end.

- Klaim: saldo ledger 1.000; retry ditolak.
- Penarikan: saldo wallet penerima 1.000, saldo ledger 0, fee 509 pradha.
- TXID simnet: `304c6d0e24afb80cec7e6e4c8fcd55e5ed7f23aaabba27f756e1cbbcbfbc6c6d`.
- Transfer ulang 1 pradha terkonfirmasi; fee 509, sisa wallet pengirim 490 pradha.
- TXID mikro simnet: `7d79f3a643c791aea7ead9387eba71616b191a7c4650d7d8dfc9a806c17b78cb`.

Output akhir: `STORAGE_FLOOR_SIMNET_PROOF_FINISHED`, exit 0. Fee aktual bergantung massa dan estimasi node; fee 509 pada fixture ini bukan tarif universal atau jaminan biaya fiat rendah.

## Pendanaan

Reservoir mainnet terakhir diverifikasi mempunyai 0 pradha on-chain. Tidak ada pengiriman dari wallet pengguna atau premine tanpa sumber/nominal yang disetujui. Pengujian transaksi menggunakan dana simnet yang dibuat oleh blok lokal. Harga CEN setara bitcoin hanya asumsi desain; bukan prediksi atau janji nilai.
