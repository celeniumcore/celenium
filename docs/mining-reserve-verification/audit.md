# Audit alokasi mining reserve dan fallback faucet

Status kandidat: BELUM LAYAK AKTIVASI MAINNET.

Scope: branch `feat/mining-floor-reserve`; mainnet tetap memakai genesis dan binary lama. Pengujian simnet membuat chain baru dengan kunci sementara, PoW dilewati oleh simnet, aturan storage mass standar tetap aktif. Tidak ada dana atau private key mainnet yang digunakan.

## Bukti selesai

- Kalkulator Python: 8 lulus.
- Consensus core: 78 lulus, 1 ignored bawaan.
- Consensus penuh: 79 lulus, 1 ignored bawaan.
- Bridge library: 150 lulus, 2 ignored hasher bawaan.
- Bridge binary: 121 lulus.
- Test regresi RED lalu GREEN: override berulang, pendanaan cadangan setelah emisi berakhir, bootstrap dengan saldo gabungan, dan perhitungan surplus dengan cadangan di dalam floor.
- Helper TestConsensus kini mengimpor UTXO genesis seperti factory node. Ekspektasi subsidi test body menggunakan konfigurasi CEN, bukan angka upstream. Dua sampel test juga gagal pada executable cache lama sebelum perbaikan; hasil ini bukan bukti build ulang baseline lama.
- Probe faucet dengan cadangan: wallet operasional nol, cadangan 500.000 CEN, alokasi umum 2.000.000 CEN. Pembayaran legacy 1 CEN dari cadangan terkonfirmasi dengan fee 509 pradha. Jurnal ditulis lalu dibaca ulang sebelum broadcast; replay memakai transaksi identik, penerima membelanjakan outputnya, rekonsiliasi masih menemukan transaksi, dan debit ledger/fee idempotent.
- Txid simnet faucet: `b6b510815d8ce00b64b6355fd559b3de52cf797d262c9d2a90be3c12595d4442`.
- Payout 1.000 pradha ditolak aturan storage mass standar; ledger klaim tetap utuh. Probe tidak menganggap penolakan ini sebagai dukungan penarikan Android.
- Probe surplus default consensus selesai: dua batch, 2 halaman riwayat, output penerima sudah dibelanjakan, dan replay tidak membayar dua kali. Txid batch pertama `91cd882ca153f905b0818e4a206092f8783d862bbd98434927805785fc2b6a60`; amount 266146010, fee 36006, dana terlindungi 250366000 pradha.
- Total suite final: 428 test Rust + 8 test Python = 436 lulus, 4 ignored bawaan. Kedua probe node exit 0. Hash seluruh source yang dilacak tetap sama sebelum/sesudah run final.

## Temuan yang menahan mainnet

1. TINGGI: pembuktian total mining 50 juta hanya berlaku untuk satu reward pada setiap indeks DAA berurutan. `mining_reserve::mining_subsidy` memakai selisih Q(d+1)-Q(d), sedangkan `difficulty::internal_calc_daa_score` dapat menduplikasi lalu melewati DAA saat beberapa blok paralel digabung. `CoinbaseManager` membayar subsidi tiap merged block. Pada batas halving DAA 1262304000, reward pada DAA 1262303999 adalah 1980518 pradha dan pada 1262304000 adalah 990260. Menggandakan yang pertama dan melewati yang kedua menambah 990258 pradha dibanding urutan ideal. Ini counterexample aritmetis atas asumsi kalkulator, bukan transaksi atau serangan live. Perlu aturan alokasi pada urutan acceptance dan uji DAG/fork sebelum menjamin suplai tepat.
2. TINGGI: cadangan diimplementasikan pada genesis baru, dibatasi simnet. Menambah output mengubah txid genesis, merkle root, UTXO commitment dan hash genesis. Belum ada migrasi/hardfork terjadwal yang mempertahankan blok, UTXO, CLTV, saldo miner dan kewajiban faucet pada mainnet berjalan. Jangan mengganti konstanta mainnet atau memakai `--yes` untuk menghapus database. Izin deploy bersyarat bukan izin reset chain.
3. TINGGI untuk penerimaan Android: minimum penarikan 1.000 pradha masih tidak dapat dibentuk dari UTXO cadangan besar dengan storage mass standar (limit 500000). Pendanaan cadangan tidak memperbaiki validitas transaksi kecil. Alternatif perlu diuji terpisah: akumulasi/batching yang valid dengan aturan lama, atau aktivasi protokol massa kecil yang terjadwal dan konsisten antarnode. Jangan menaikkan nominal klaim atau mengaktifkan override simnet secara diam-diam.
4. SEDANG: satu review independen fallback gagal HTTP 429. Review tersebut tidak dihitung lulus. Dua review subagent lain selesai, tetapi temuan dan hasil eksekusi tetap diverifikasi parent; ini bukan audit pihak ketiga.
5. SEDANG: field `pendingTxid` dari bridge belum dibaca parser Android yang membaca `txid`. Uji kontrak pending/unknown perlu dilengkapi sebelum rilis aplikasi.
6. SEDANG: dua output genesis eksperimen memakai alamat alokasi umum dan reserve terpisah. Probe operasional nol menguji fallback saja. Deployment lengkap harus mengikat alokasi umum ke wallet floor/operasional yang diawasi dan signer yang tepat, serta memeriksa ketiga proses melalui service/HTTP setelah restart.
7. TINGGI untuk gate pemulihan: reload file dan settlement idempotent pada probe belum membuktikan restart proses bridge atau rekonsiliasi otomatis `treasury::cycle`. Probe memanggil settlement dan membersihkan jurnal secara manual. Uji crash sebelum broadcast, setelah broadcast, setelah penyimpanan ledger, dan sebelum pembersihan jurnal melalui proses/HTTP sebenarnya masih diperlukan; setiap skenario harus menghasilkan satu pembayaran dan satu debit.
8. TINGGI untuk gate upgrade: ledger dan jurnal memakai path relatif terhadap working directory; loader menganggap file hilang sebagai state kosong. Belum ada bukti migrasi memakai salinan ledger/jurnal versi lama dengan pending withdrawal dan kewajiban di atas cap. Pertahankan saldo, device mapping, kuota, fee, settled txids dan working directory systemd. Jangan menimpa ledger aktif dengan backup lama setelah transaksi diterima.
9. SEDANG: serializer override baru menghasilkan field `mining_reserve`, sementara parser lama memakai `deny_unknown_fields`; round-trip versi baru tidak membuktikan downgrade. Konversi header ke `GenesisBlock` juga menetapkan reserve `None`. Perlu fixture config lama-ke-baru, aturan downgrade eksplisit, dan validasi helper import genesis reserve.

## Batas bukti snapshot

Suite dan log di direktori ini berasal dari source runtime yang dibekukan lalu dicommit sebagai `7703767`. Angka 436 adalah jumlah hasil lulus dari suite yang disebutkan, bukan klaim seluruh workspace atau semua acceptance rilis sudah lulus. Hash artifact pada `summary.json` adalah hash pada saat run tersebut, bukan jaminan hash file cache build pada waktu lain.

Pemeriksaan tambahan sempat mencoba melarang withdrawal memakai buffer/cadangan layanan. Perubahan itu dibatalkan sebelum commit/push karena cadangan layanan memang boleh membayar klaim; perlindungan terhadap sweep surplus berbeda dari larangan memakai dana untuk payout. Setelah pembatalan, SHA256 setiap source Rust dicocokkan dengan manifest run 436 test dan hasilnya identik. Run eksperimen tambahan tidak menggantikan bukti snapshot yang dilampirkan.

## Mekanisme yang tetap wajib

- Hanya worker bootstrap operator diarahkan ke floor bila saldo eligible gabungan di bawah target; worker lain tetap ke alamatnya.
- Floor dinamis berasal dari bulan DAA node. Cadangan 500 ribu CEN termasuk target total 2,5 juta CEN.
- Surplus hanya mengurangi sumber operasional setelah gabungan saldo melindungi floor, hak klaim, buffer dan cadangan layanan. Jurnal transaksi menahan pembayaran baru sampai rekonsiliasi.
- Kewajiban lama tidak dipotong saat plafon atau target turun. Cadangan nyata tetap bisa mendukung layanan setelah emisi berhenti; target numerik tidak pernah dianggap dana.

## Mainnet yang diamati

Pembacaan sebelum perubahan: node dan bridge aktif. Bridge executable `/proc/1010781/exe` dan binary terpasang sama SHA256 `0483d47c50b7e84aa0018cee951770998f4621f1bbce222ae071f5e4f7301a02`.
API publik menunjukkan floor awal 250000000000000 pradha, mature reservoir 0, outstanding 25000001000, tidak ada transaksi surplus pending/terakhir, total pengembalian 0. Alamat bootstrap dan surplus cocok dengan kas miner operator. Ini bukti konfigurasi dan kode aktif, bukan bukti reservoir terdanai atau surplus pernah dikirim.

## Gate penerapan

Tidak ada deploy consensus/mainnet sampai temuan tinggi ditutup dan diuji, termasuk multi-node/DAG/reorg, kompatibilitas history, payout nominal Android, dan jalur pemulihan. Tidak melakukan release build untuk aktivasi ketika desain protokol masih gagal audit. Branch eksperimen boleh dipush untuk review, tanpa merge ke main.
