# Alokasi cadangan mining untuk opsi genesis baru

Kalkulator ini memodelkan **genesis baru yang dipilih secara lokal (opt-in)**.
Alokasi genesis yang berubah menghasilkan hash genesis berbeda dan memerlukan
jaringan/chain baru yang sengaja dikonfigurasi. Menjalankan skrip hanya membaca
tabel Rust dan mencetak laporan; skrip tidak mengubah konsensus, membuat UTXO,
mengirim transaksi, melakukan deploy, atau mengaktifkan perubahan mainnet.
Angka di bawah adalah hasil model emisi, bukan bukti dana live atau saldo wallet.

## Pembagian suplai

Cadangan mining awal berasal dari jatah mining nominal 52.500.000 CEN.
Premine CLTV 16.500.000 CEN beserta ketentuan pengunciannya tetap.

| Alokasi | CEN | Waktu alokasi dalam model |
|---|---:|---|
| Cadangan umum untuk floor/reservoir | 2.000.000 | Genesis baru |
| Cadangan faucet | 500.000 | Genesis baru |
| Total cadangan mining awal | 2.500.000 | Genesis baru, subtotal dua baris di atas |
| Premine CLTV pengembangan | 9.600.000 | Genesis, tetap terkunci sesuai jadwal |
| Premine CLTV pemilik | 6.900.000 | Genesis, tetap terkunci sesuai jadwal |
| Total alokasi pada genesis baru | 19.000.000 | Cadangan mining + premine CLTV |
| Mining masa depan | 50.000.000 | DAA 1 sampai sebelum akhir emisi |
| Suplai akhir nominal | 69.000.000 | Genesis + mining masa depan |

Dalam pradha (`1 CEN = 100000000 pradha`):

```text
cadangan awal + mining masa depan = 250000000000000 + 5000000000000000
                                  = 5250000000000000
premine CLTV + cadangan awal      = 1650000000000000 + 250000000000000
                                  = 1900000000000000
genesis baru + mining masa depan  = 1900000000000000 + 5000000000000000
                                  = 6900000000000000
```

Cadangan faucet merupakan bagian dari cadangan 2,5 juta CEN. Jangan menambahkannya
lagi ke total. Target floor adalah kebijakan jumlah dana yang ingin dipertahankan;
target itu sendiri tidak mencetak koin atau membuktikan adanya UTXO.

## Sumber legacy dan pembulatan

Skrip membaca `SUBSIDY_BY_MONTH_TABLE` dari
`consensus/core/src/config/emission.rs`. Bila file itu belum ada, skrip membaca
`consensus/src/processes/coinbase.rs`. File pertama yang ada harus valid; parser
tidak diam-diam menggunakan tabel lama jika file baru rusak.

Tabel memiliki 817 entri: 17 era dengan 48 bulan per era dan satu entri nol.
Parser memeriksa jumlah entri, literal integer, rentang `u64`, nol terakhir,
konstansi setiap era, dan rasio halving nominal dengan toleransi pembulatan
integer. Tes mengunci semua nilai legacy yang diharapkan, termasuk checksum
SHA-256 atas angka desimal yang digabung dengan koma tanpa spasi/newline:

```text
30fc40c35ba310e14cbe98eb0e1914d1ff09330a9133e5f978005eb1c7da9a66
```

Seluruh perhitungan memakai integer Python, termasuk pemformatan CEN. Skrip tidak
membangkitkan ulang tabel dengan float dari `calculate-emission.py`.

```text
BPS = 10
seconds_per_month = 2629800
blocks_per_month = 26298000
legacy_reward(d) = ceil(table[d // blocks_per_month] / 10)
                = (table[d // blocks_per_month] + 9) // 10
```

Reward legacy menjadi nol saat indeks bulan mencapai 816. Total berbobot tabel
`Wtotal = sum(ceil(rate/10) * blocks_per_month)` adalah
**52.500.081,72672000 CEN**, atau `5250008172672000` pradha. Selisih dari nominal
52.500.000 CEN adalah **+81,72672000 CEN**, dengan rincian:

| Perhitungan legacy | CEN |
|---|---:|
| Total tabel pada 1 BPS | 52.499.999,67696000 |
| Deviasi tabel 1 BPS terhadap nominal | -0,32304000 |
| Tambahan akibat pembulatan ke atas pada 10 BPS | +82,04976000 |
| Total berbobot 10 BPS | 52.500.081,72672000 |
| Total berbobot + premine CLTV | 69.000.081,72672000 |

`Wtotal` mencakup bobot legacy pada indeks nol untuk mendefinisikan kurva. Ini
bukan pengukuran suplai beredar atau klaim bahwa reward DAA nol telah dibayarkan
on-chain. Model baru menghapus bobot itu dari normalisasi mining masa depan.

## Jadwal integer yang tepat

Definisikan `n` sebagai batas eksklusif indeks DAA. Semua indeks nonnegatif.

```text
end = 816 * blocks_per_month = 21459168000
W(n) = jumlah legacy_reward(d) untuk 0 <= d < min(n, end)
offset = legacy_reward(0) = 2079547
D = Wtotal - offset = 5250008170592453
T = 50000000 * 100000000 = 5000000000000000
Q(n) = (max(0, W(n) - offset) * T) // D

new_reward(d) = 0                       jika d = 0 atau d >= end
new_reward(d) = Q(d + 1) - Q(d)         jika 1 <= d < end
```

`Q(n)` adalah total mining baru pada indeks `[1, n)`, tanpa cadangan genesis.
`Q(0) = Q(1) = 0`, sehingga tidak ada emisi mining tambahan saat genesis.
Cadangan 2,5 juta CEN adalah alokasi tersendiri pada genesis baru.

Batas bulan tetap `m * 26298000`; jadwal tidak digeser satu blok. Halving nominal
setiap 48 bulan tetap berasal dari tabel yang sama. Reward integer dapat berganti
antara dua nilai yang berjarak satu pradha dalam era yang sama, sehingga setiap
pasangan blok tidak harus tepat berbanding dua pada halving. Emisi selesai pada
awal bulan 816, yaitu 68 tahun nominal dalam definisi bulan konsensus; ini bukan
janji tanggal kalender jaringan.

| DAA | Reward baru (pradha) | Q(DAA), batas eksklusif (pradha) |
|---:|---:|---:|
| 0 | 0 | 0 |
| 1 | 1980517 | 0 |
| 2 | 1980518 | 1980517 |
| 26298000 | 1980518 | 52083656967224 |
| 1262304000 | 990260 | 2500015627511132 |
| 21459167999 | 31 | 4999999999999969 |
| 21459168000 | 0 | 5000000000000000 |

### Konservasi dan batas reward

Untuk interval `[a, b)` dengan `a >= 1`, penjumlahan reward meniadakan suku-suku
kumulatif di tengah dan menghasilkan `Q(b) - Q(a)`. Karena `Q(1) = 0` dan
`Q(end) = T`, jumlah seluruh reward masa depan tepat `5000000000000000` pradha.
Tidak ada pembulatan total yang ditoleransi atau pembayaran tambahan di akhir.

Tuliskan `alpha = T/D`. Karena `0 < T <= D`, untuk reward legacy integer `r`:

```text
floor(alpha * r) <= Q(d + 1) - Q(d) <= ceil(alpha * r) <= r
```

Maka tidak ada reward mining baru per blok yang melebihi reward legacy pada DAA
yang sama. Ini berlaku pada seluruh blok, bukan hanya sampel awal/akhir bulan.

Laporan membuktikan total lewat 817 interval bulanan. Untuk bulan pertama,
interval yang dihitung mulai DAA 1. Baris bulan 816 tetap disertakan sebagai
interval nol. `scheduled_blocks` menghitung posisi DAA dalam interval, termasuk
posisi dengan reward nol; ia bukan jumlah blok dengan reward positif.

Setiap baris memuat batas reward `reward_floor_pradha` dan `reward_ceil_pradha`,
serta jumlah blok masing-masing. Bila `lo = floor(r*T/D)`, jumlah blok yang
menerima `lo+1` adalah `total_interval - lo * scheduled_blocks`. Jika pembagian
habis, semua blok masuk kelompok floor dan kelompok ceil berjumlah nol. Dengan
histogram ini, konservasi dapat diverifikasi tanpa menelusuri miliaran DAA.

## Target floor dan faucet

Target dievaluasi pada **awal bulan legacy**, mengikuti kebijakan bulanan.
Untuk DAA tertentu, gunakan `month = daa // blocks_per_month`:

```text
remaining_legacy(month) = Wtotal - W(month * blocks_per_month)
target_floor(month) = (250000000000000 * remaining_legacy(month)) // Wtotal
faucet_target(month) = min(target_floor(month), 50000000000000)
```

Rasio ini memakai bobot legacy penuh sebagai pembanding awal, bukan denominator
`D` yang mengecualikan DAA nol dan bukan sisa mining baru `T-Q(n)`. Target awal
adalah 2.500.000 CEN total, dengan porsi faucet paling banyak 500.000 CEN. Pada
bulan 48 target floor menjadi 1.249.992,18574930 CEN. Pada bulan 144, floor dan
target faucet sama-sama 312.485,87423913 CEN. Keduanya nol pada bulan 816.

Target-target ini bukan tambahan suplai, jaminan saldo, nominal per klaim, atau
bukti dana siap dibelanjakan. Pencadangan UTXO, hak signer, penguncian, kewajiban
ledger, dan pembayaran faucet harus diverifikasi pada implementasi jaringan
baru secara terpisah.

## Menjalankan dan menguji

Python 3.9 atau lebih baru, hanya pustaka standar. Dari root repo:

```bash
python3 -B scripts/calculate-mining-reserve.py
python3 -B scripts/calculate-mining-reserve.py --json
python3 -B scripts/calculate-mining-reserve.py --csv
python3 -B -m unittest discover -s scripts/tests -p test_mining_reserve.py -v
```

Tanpa flag atau dengan `--json`, stdout berisi laporan lengkap, pembagian suplai,
deviasi legacy, hasil verifikasi, dan semua interval. `--csv` mencetak hanya
interval bulanan ke stdout. Kedua flag saling eksklusif. Skrip menyelesaikan
verifikasi konservasi dan batas reward sebelum mencetak hasil; exit nonnol
menandakan input/hasil tidak valid. Lokasi tabel ditentukan relatif terhadap
skrip, sehingga perintah juga dapat dijalankan dari direktori lain.

Field `_pradha` berisi integer. Field `_cen` pada laporan ringkasan berupa string
desimal delapan angka agar konsumen tidak perlu mengubah nilai ke float. Tes
memeriksa nilai sumber yang tepat, batas DAA, semua interval bulanan, target
cadangan, pemisahan nominal/genesis, penolakan input tidak valid, dan kesesuaian
JSON/CSV. Pengembangan dilakukan dengan siklus tes RED lalu GREEN. Verifikasi
Python ini tidak menggantikan tes konsensus Rust atau verifikasi node/on-chain.
