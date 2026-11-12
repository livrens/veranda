# Cara memakai Veranda

Panduan setiap layar. English: [USAGE.md](USAGE.md).

Veranda punya dua bagian. **Stack lokal** (Overview, Sites, Database, Logs, dan halaman tiap
layanan) menjalankan web server, bahasa, dan database di komputer Anda sendiri. Grup **Remote**
adalah klien SSH untuk server yang Anda kelola di tempat lain. Tidak ada data yang keluar dari
komputer kecuali Anda sendiri menghubungkan ke server pilihan Anda; tanpa akun, tanpa telemetri.

## 1. Pertama kali

1. Pasang Veranda ([README.id](../README.id.md#pemasangan)) lalu buka. Jendela dibuka di
   **Overview**, yang kosong pada pemasangan baru.
2. Di sidebar, klik layanan yang Anda butuhkan (misalnya **PHP**, **Nginx**, **MariaDB**). Setiap
   halaman layanan punya dua tab: **Service** (status, port, tombol) dan **Versions** (daftar
   versi yang bisa diunduh). Pilih versi lalu klik **Install**. Unduhan diperiksa dengan checksum
   dari vendor.
3. Nyalakan saklar di samping layanan. Layanan langsung berjalan dan tampil di Overview.
4. Buka <http://localhost/> (atau **Start All** di Overview untuk menjalankan semua yang aktif).

Semua data stack lokal ada di satu folder, **root Veranda** (`~/veranda` secara default, tampil
di Settings). Menghapus folder itu menghapus semua runtime, situs, dan data.

## 2. Overview

**Grup startup** (chip di atas daftar layanan) menjalankan dan menghentikan sekumpulan layanan
dengan satu klik, sesuai urutan yang Anda pilih: misalnya "Laravel stack" = Nginx, PHP, MariaDB.
Grup yang ditandai default dijalankan saat Veranda dibuka jika "Start services on launch"
aktif.


- Pil status menunjukkan apakah semua layanan yang aktif sedang berjalan.
- **Start All / Stop All** bekerja pada semua layanan yang saklarnya menyala.
- Tiap baris menampilkan versi yang dipakai (menu pilihan jika ada beberapa), port, dan tombol
  start/stop/restart. Saklar mematikan layanan: dihentikan dan disembunyikan dari Overview sampai
  dinyalakan lagi dari sidebar.
- Ubin membuka web root, Adminer, penampil email, terminal di document root, folder `www`, dan
  Logs.

Jika port sudah dipakai, Veranda menyebut program pemakainya dan menawarkan port bebas berikutnya.
Di Linux, port di bawah 1024 butuh izin sekali; Veranda meminta kata sandi Anda satu kali dan
memberi izin itu hanya ke binari web server (atau ganti ke 8080/8443 di Settings).

## 3. Sites


Setiap folder di dalam `www/` otomatis menjadi `https://nama-folder.test`. Folder `public/` gaya
Laravel dan Symfony terdeteksi sendiri. Untuk proyek di tempat lain, pakai **Add external folder**
(ditautkan di tempat) atau **Migrate** untuk memindahkan proyek dan database dari Laragon, XAMPP,
atau FlyEnv. Migrate mencari tiap stack di lokasi biasanya; untuk FlyEnv dibaca daftar host
(`host.json`) atau berkas vhost Apache/Nginx di folder datanya, dan Veranda memberi tahu bila
FlyEnv terpasang tetapi belum punya situs (pakai "Other folder of projects" lalu tunjuk folder
proyek Anda). **New project** membuat folder dari templat (polos, paket Composer lewat `composer` sistem, Git clone) dan,
jika dicentang, database. **Sync** menulis ulang virtual host dan berkas hosts (minta izin
elevasi sekali).

## 4. Database

Host, pengguna, dan kata sandi server MariaDB/MySQL lokal, **New password** sekali klik, buat
database, buka Adminer atau phpMyAdmin, dan **Backup** (dump semua database ke `backups/`; backup
otomatis diatur di Settings). PostgreSQL punya kartu yang sama di halamannya sendiri.

Veranda memberi `root` password acak saat pertama dijalankan. Proyek yang dibuat untuk Laragon
atau XAMPP biasanya mengharapkan `root` tanpa password: tempel password dari kartu ini ke `.env`
proyek, atau klik **Tanpa password**. Server hanya mendengarkan di `127.0.0.1`, jadi root tanpa
password tetap hanya bisa diakses dari komputer Anda sendiri.

### Berpindah antara MariaDB dan MySQL
Tiap mesin menyimpan datanya di folder sendiri (`data/mariadb`, `data/mysql`), jadi setelah mesin
diganti di sidebar, database Anda seolah hilang; sebenarnya tidak. **Sites → Migrate**
menampilkan pilihan *Veranda MariaDB → MySQL (databases stored here)* (atau sebaliknya). Pilih
lalu tekan **Migrate now**: Veranda menjalankan mesin yang tersimpan di port kosong, menyalin
tiap database ke mesin yang sedang berjalan dengan alat dump milik mesin itu sendiri, lalu
mematikannya lagi. Folder data lama tidak diubah, jadi Anda bisa kembali kapan saja. Database
bernama sama di mesin yang berjalan akan diganti. Bila kata sandi root diubah setelah berpindah,
mesin lama masih memakai kata sandi sebelumnya; ketik di kolom kata sandi.

Dialog menampilkan persentase (berbobot ukuran database), database yang sedang disalin, dan
byte yang mengalir. Jendelanya boleh ditutup; tombol **Migrate** menampilkan kemajuannya dan
membuka kembali dialog beserta hasilnya. **Jeda** menahan salinan (tidak ada yang hilang saat
dilanjutkan); **Batalkan setelah database ini** menyelesaikan database yang sedang disalin lalu
berhenti, dan tekanan kedua, **Hentikan sekarang**, memutus salinan berjalan (database itu
disalin ulang lain kali). Setiap hasil juga dicatat di `logs/migrate.log`.

## 5. Logs

Kotak **Activity** mencatat apa yang Veranda lakukan (start, stop, pasang, error). Menu pilihan
membuka berkas log tiap layanan; error dan peringatan diberi warna.

## 5b. Berkas
Editor kecil untuk file proyek, di sidebar di bawah Log (dan ikon pensil di setiap baris Sites).
Pilih folder (document root, folder paket pindahan, home, atau folder lain) lalu sebuah file.
Markdown dibuka berdampingan dengan pratinjau (judul, daftar, daftar tugas, tabel, blok kode,
tautan; tautan luar dibuka di browser, tautan relatif membuka filenya), CSV sebagai tabel, JSON
dengan pemeriksaan validitas dan tombol **Format**. Kode diwarnai untuk PHP,
JavaScript/TypeScript, Python, Go, Rust, Java, bahasa mirip C, Ruby, SQL, shell, CSS, HTML/XML,
YAML, TOML, INI, .env, Dockerfile, dan Makefile. Ctrl+S menyimpan (akhiran baris dipertahankan),
permission file dijaga, dan file yang berubah di disk tidak ditimpa tanpa bertanya. Klik kanan
entri di pohon untuk mengganti nama atau menghapus. File di atas 4 MB dan file biner tidak dibuka.

## 6. Settings

Umum (bahasa, TLD, document root, editor, jalan saat dibuka, mulai diminimalkan, perilaku tombol
tutup, rendering GPU di Linux), port web dan HTTPS, batas PHP, port database/cache/mail, backup,
dan pembaruan. **Trust local CA** memasang otoritas sertifikat Veranda ke penyimpanan browser agar
`https://*.test` tidak memberi peringatan. **Config file** membuka `veranda.toml` untuk pengaturan
yang tidak ada di formulir.

### Tampilan
**Tema** mengikuti sistem atau memaksa terang/gelap. **Gaya** memilih palet: Beranda (bawaan
yang hangat), Kaca (permukaan tembus pandang di atas sapuan warna lembut), Kertas (datar, tinta
di atas krem), atau Arang (abu-abu netral beraksen kuningan). Keduanya disimpan di komputer ini
dan langsung diterapkan.

### Keamanan: kunci aplikasi
**Atur PIN atau kata sandi** meminta PIN (angka) atau kata sandi (teks apa saja, minimal 4
karakter); sejak itu Veranda memintanya saat dibuka dan setelah waktu diam yang dipilih di
**Kunci setelah** (atau hanya saat tombol kunci di sidebar ditekan). Layanan tetap berjalan
selama terkunci; jendela dikaburkan dan backend menolak perintah sampai rahasia yang benar
dimasukkan. Setelah lima kali salah Anda harus menunggu, makin lama tiap kali. Yang disimpan
hanya hash bergaram (`etc/secrets/app-lock.json`); tidak ada pemulihan, jadi bila lupa hapus
berkas itu saat sudah masuk ke komputer ini. Kunci ini menjaga Veranda sendiri; untuk melindungi
berkas proyek dan database pada laptop yang dicuri, nyalakan enkripsi disk (BitLocker di
Windows, FileVault di macOS, LUKS di Linux) dan kata sandi masuk.

**Dua faktor** (opsional, setelah kunci aktif): **Aktifkan** menampilkan kunci penyiapan untuk
aplikasi autentikator (Aegis, Google Authenticator, Authy, 1Password, Bitwarden…); ketik kunci
itu di aplikasi, lalu kode 6 digit yang ditampilkannya. Sejak itu layar kunci meminta PIN atau
kata sandi plus kode saat ini. Simpan kuncinya di tempat aman: tanpa aplikasi dan kunci itu,
kunci aplikasi tidak bisa dibuka (jalan terakhir: hapus `etc/secrets/app-lock.json` saat sudah
masuk). Semuanya berjalan offline; tanpa nomor telepon, tanpa server.

### Modul
Inti Veranda adalah tumpukan server lokal. **Remote** (SSH), **Files** (editor), dan **Pindah ke
komputer lain** adalah modul yang bisa dimatikan di sini; keduanya hilang dari sidebar dan
halaman sampai dinyalakan lagi. Tidak ada yang dicopot atau dihapus. Aturan untuk apa yang boleh
masuk Veranda: harus menyentuh proyek lokal Anda atau server tempat proyek itu berjalan.

### Penyimpanan
Kartu **Penyimpanan** menampilkan isi folder Veranda (paket, database, backup, log, proyek).
**Kosongkan ruang sekarang** membuang build debug, test suite, library statis, halaman man, dan
header dari paket yang terpasang (MySQL saja membawa sekitar 550 MB berkas seperti itu),
mengompresi backup SQL lama, menghapus sisa unduhan, dan memangkas log di atas 20 MB. Database,
proyek, dan pengaturan tidak pernah disentuh. Paket baru diramping dengan cara yang sama saat
dipasang ("Paket ramping" di General). Backup ditulis terkompresi gzip (`.sql.gz`); `veranda db
import` dan paket pindahan membaca kedua bentuk. Dari terminal: `veranda clean`.

### Pindah ke komputer lain
**Buat paket pindahan** mengemas semua yang bersifat pribadi ke satu file `.veranda-move` yang
dilindungi kata sandi (PBKDF2 + AES-256-GCM): pengaturan Veranda, situs, CA lokal, host remote,
kunci, snippet beserta kata sandinya; proyek di document root (tanpa `node_modules`, `vendor`,
`target`, dan sejenisnya kecuali Anda centang "Ikutkan"); `~/.ssh`; konfigurasi dan kredensial
git; setelan, keybinding, dan snippet VS Code; setelan Claude Code, CLAUDE.md, skill, agent,
command, plugin, memori proyek, dan login (tidak pernah transkrip sesi atau cache); serta folder
tambahan yang Anda pilih (**Cari proyek** menampilkan semua folder git, Composer, atau npm di
folder home dan drive yang terpasang; daftar pilihan Anda diingat untuk paket berikutnya). Paket
juga mencatat paket Veranda yang terpasang, repository git (remote dan branch), ekstensi VS
Code, dan tools developer yang ada di `PATH`. Setelan per proyek (`.vscode/`, `.claude/`,
`CLAUDE.md`, `.env`) ikut di dalam folder proyeknya, dan memori per proyek Claude Code
dihubungkan ulang ke path baru folder itu setelah pemulihan. **Database** menambahkan dump SQL
segar dari database MariaDB/MySQL Anda saat server berjalan (kalau tidak, backup terbaru buatan
Veranda); PostgreSQL belum termasuk.

Pemulihan tidak pernah menghapus apa pun. File yang sudah ada di komputer baru dipindahkan ke
`backups/move-<tanggal>` sebelum salinan dari paket menggantikannya, dan file yang tidak ada di
paket tetap seperti semula. Jadi memulihkan ke komputer yang sudah berisi data berarti
menggabungkan keduanya: file bernama sama diambil dari paket, sisanya dipertahankan. Dump
database dimuat ke database yang berjalan (dinyalakan bila perlu) oleh langkah penyelesaian, atau
nanti dengan `veranda db import <file>`; database bernama sama ditimpa oleh dump.

Paket adalah potret sesaat, bukan sinkronisasi: buat yang baru tepat sebelum pindah komputer.

Di komputer baru, pasang Veranda, buka Settings, **Pulihkan paket**, pilih file dan kata
sandi, centang bagian yang diinginkan, lalu tekan Pulihkan. File yang sudah ada dipindahkan dulu
ke `backups/move-<tanggal>`. Setelah itu Veranda memasang ulang versi paket yang sama (atau yang
paling dekat untuk platform itu), meng-clone repository yang belum ada, memasang ekstensi VS Code
bila `code` ada di `PATH`, dan di Windows bisa memasang tools yang hilang lewat `winget`.
Programnya sendiri tidak pernah ikut: biner Linux tidak bisa jalan di Windows.

Dari terminal juga bisa: `veranda move create <file>` (tambah `--dry-run` untuk melihat ukuran,
`--folder <path>` untuk folder tambahan, `--only ssh --only git` untuk membatasi; `veranda move
suggest` menampilkan folder proyek), `veranda move inspect <file>`, dan `veranda move restore <file>`. Kata sandi diambil dari
`VERANDA_MOVE_PASSWORD` atau ditanyakan. Kata sandi yang terlupa tidak bisa dipulihkan.

## 7. Remote (klien SSH)

### Hosts
Klik **New host**: label, alamat, port, nama pengguna, dan cara masuk (**Auth**):

| Auth | Artinya |
| --- | --- |
| Password | Memakai kata sandi yang Anda ketik di sini (disimpan di penyimpanan kredensial sistem) atau ditanya saat menghubungkan |
| Key | Kunci dari Keychain |
| Identity | Nama pengguna + kata sandi/kunci dari Keychain yang bisa dipakai ulang |
| SSH agent | Kunci yang sudah dimuat di agent (`ssh-agent`, OpenSSH agent Windows, Pageant) |
| Ask when connecting | Tanpa kredensial tersimpan; prompt dari server ditampilkan |

Opsional: **Group** (host mewarisi identitas grup), **Jump host** (lewat host lain dulu),
**Startup snippet** (dikirim setelah login), tag, warna, variabel environment, catatan,
keep-alive, dan agent forwarding.

Klik dua kali host untuk membuka terminal. Tombol di baris: terminal, SFTP, ubah, duplikat, hapus.
**Quick connect** (`user@host:22`) menghubungkan tanpa menyimpan. **Import** membaca
`~/.ssh/config` (dan sesi PuTTY di Windows) lalu menambahkan hostnya, berikut kuncinya jika
"Import private keys" dicentang.

Saat pertama terhubung ke server, sidik jari kuncinya ditampilkan. Bandingkan dengan yang
diumumkan admin server, lalu **Trust**. Server yang kuncinya berubah ditandai merah; lanjutkan
hanya jika Anda tahu sebabnya (server dipasang ulang). Kunci yang diterima ada di **Known hosts**
dan bisa dilupakan di sana.

### Terminal
Tab menampung sesi SSH dan lokal (**+** atau Ctrl+Shift+T membuka shell lokal dengan PHP, Node,
dan lainnya milik Veranda sudah ada di `PATH`). Di bar: status, **Snippets**
(Ctrl+Shift+S membuka panel di samping terminal: cari, klik nama untuk mengetikkannya ke sesi,
dan panel tetap terbuka sampai Anda menutupnya), cari (Ctrl+Shift+F), bersihkan, hubungkan
ulang, pengaturan, tutup. Ctrl+Shift+C/V salin dan tempel; memilih teks otomatis menyalin jika
"Copy on select" aktif; klik kanan menempel. Ikon gerigi mengatur font, ukuran, tema (ikuti
Veranda, gelap, terang, One Dark, Tokyo Night, Catppuccin, Dracula, Gruvbox, Monokai, Solarized,
Nord), kursor, scrollback, interval keep-alive, program shell lokal, dan konfirmasi saat menutup
sesi.

#### Statistik server
Di bawah bilah terminal SSH ada strip tipis berisi **CPU**, **RAM**, dan **Disk** server beserta
uptime-nya. Disk adalah filesystem lokal yang paling penuh (yang habis lebih dulu; nama mount
ditampilkan bila bukan `/`); arahkan kursor untuk melihat tiap mount dan total semua disk yang
biasanya dilaporkan konsol cloud, itulah sebabnya angka di sana bisa lebih rendah. Nilainya dibaca
dari `/proc` dan `df` lewat koneksi yang sudah ada tiap 10 detik selama terminal itu terlihat;
tidak ada yang berjalan saat tab tersembunyi. Klik strip untuk menyembunyikannya; pengaturan
terminal (ikon roda gigi) punya kotak centang untuk menampilkannya lagi.

#### Jendela kecil
Saat area terminal lebih sempit dari sekitar 640 px, panel snippet melayang di atas terminal
alih-alih menjepitnya; klik di terminal menutupnya. Bilah atas membuang nama host dan label
tombol lebih dulu, baru teks status.

### SFTP
Pilih host lalu **Connect**. Panel kiri komputer ini, panel kanan server. Klik dua kali folder
untuk masuk, pakai kotak jalur, **↑**, home, dan segarkan. Pilih file (Ctrl atau Shift untuk
banyak) lalu tekan panah di antara panel, atau seret ke panel lain, untuk unggah/unduh; folder
dipindahkan beserta isinya. Daftar transfer menampilkan progres dan tombol batal. Tombol di baris:
ganti nama, hapus, ubah izin (server), dan buka file teks di editor kecil (server).

### Port forwarding
**New rule** dengan jenis:

- **Local**: `127.0.0.1:3307 → 127.0.0.1:3306` lewat server membuat alat lokal menjangkau
  database server di port 3307.
- **Remote**: server mendengarkan di sebuah port dan meneruskan koneksi ke layanan di komputer
  Anda.
- **Dynamic**: proxy SOCKS5 di komputer Anda; arahkan browser ke sana untuk berselancar lewat
  server.

Saklar menjalankan dan menghentikan aturan; baris menampilkan jumlah koneksi terbuka. Biarkan
alamat bind di `127.0.0.1` kecuali Anda memang ingin mesin lain di jaringan memakai tunnel itu.

### Snippets
Perintah yang sering dipakai, dikelompokkan per folder. Daftar hanya menampilkan nama, tidak
pernah isi perintahnya; isi tampil di editor. **Send** mengetikkannya ke terminal aktif (panel
Snippets di terminal melakukan hal yang sama dengan sekali klik); **Run on hosts** menjalankannya
di host yang dicentang dan menampilkan keluaran serta kode keluar tiap host berdampingan.

**Pasang kata sandi** mengunci halaman ini dan panel di terminal sampai kata sandi dimasukkan,
dan mengenkripsi isi setiap snippet di disk (AES-256-GCM, kunci diturunkan dari kata sandi dengan
PBKDF2). Status terbuka bertahan sampai Veranda ditutup, sampai Anda menekan **Kunci**, atau
setelah waktu diam yang dipilih. Startup snippet dan "Run on hosts" juga butuh kunci terbuka.
Tidak ada pemulihan: kata sandi yang terlupa berarti isi snippet hilang, jadi simpan kata sandi
di tempat aman.

### Keychain
**New key** membuat kunci Ed25519 (disarankan), RSA 4096, atau ECDSA, opsional dengan passphrase.
**Import key** membaca file dari `~/.ssh` atau teks yang ditempel. Per kunci: salin kunci publik,
**Install on host** (menambahkannya ke `authorized_keys` host, seperti `ssh-copy-id`), ekspor file
publik atau privat, ganti passphrase, hapus. **Identities** menggabungkan nama pengguna dengan kata
sandi atau kunci agar beberapa host dan grup berbagi satu login.

### History
Setiap koneksi dengan waktu, durasi, dan hasilnya; klik tombol terminal untuk terhubung lagi.

## 8. Pembaruan

Instalasi Windows, macOS, dan AppImage Linux memperbarui diri lewat feed updater bertanda tangan.
Instalasi **.deb dan .rpm di Linux** juga bisa: Veranda mengunduh paket dari rilis, memeriksa
tanda tangannya dengan kunci yang sama, memasangnya lewat dialog kata sandi sistem
(polkit/`pkexec`), lalu memulai ulang. Jika `pkexec` tidak ada, banner memberi tahu lokasi paket
yang sudah diunduh agar bisa dipasang dengan `sudo dpkg -i` atau `sudo rpm -U`.

Veranda memeriksa rilis GitHub proyek ini (github.com/livrens/veranda) 8 detik setelah dibuka, tiap jam, dan saat jendela kembali tampil setelah 30 menit atau lebih, lalu
menampilkan spanduk; **Update now** mengunduh installer bertanda tangan dan memulai ulang. Dengan
**Install updates automatically** aktif, unduhan dan mulai ulang terjadi sendiri: saat opsi itu
dinyalakan, pada tiap pemeriksaan, dan setelah **Check for updates**. Pembaruan mandiri berjalan
untuk .deb, .rpm, dan AppImage di Linux, installer NSIS/MSI di Windows, dan .app di macOS;
spanduk memberi tahu bila sebuah pemasangan tidak bisa memperbarui dirinya (pakai halaman rilis).
Versi layanan (PHP, MariaDB, dan lainnya) diambil dari server vendor masing-masing di tab
Versions.

## 9. Di mana data Anda

| Data | Lokasi |
| --- | --- |
| Pengaturan | `<root>/etc/veranda.toml` |
| Runtime terpasang | `<root>/bin/<jenis>/<versi>` |
| Situs | `<root>/www` |
| Database | `<root>/data` |
| Log, backup | `<root>/logs`, `<root>/backups` |
| Otoritas sertifikat lokal | `<root>/etc/ssl` (privat) |
| Kata sandi root database | `<root>/etc/secrets` (privat) |
| Host remote, grup, snippet, aturan, riwayat | `<root>/etc/remote/store.json` (privat; isi snippet terenkripsi bila kata sandi snippet dipasang) |
| Pemeriksa kata sandi snippet (salt + verifier, bukan kuncinya) | `<root>/etc/remote/snippet-lock.json` (privat) |
| Kunci privat SSH | `<root>/etc/remote/keys` (privat, 0600) |
| Kunci server yang diterima | `<root>/etc/remote/known_hosts` |
| Kata sandi dan passphrase SSH | penyimpanan kredensial sistem (Keychain, Credential Manager, Secret Service); cadangan `<root>/etc/secrets/remote-secrets.json` |

Cadangkan folder root untuk menyimpan semuanya; jangan pernah membagikan folder `etc/secrets`
atau `etc/remote/keys`.

## 10. Pemecahan masalah

- **phpMyAdmin bilang "the mysqli extension is missing"**: versi PHP itu dipasang dengan bundel
  lama yang lebih kecil. Buka halaman PHP, hapus versi tersebut lalu pasang lagi.
- **Layanan tidak mau jalan**: buka Logs, pilih log layanan itu. Baris terakhir menjelaskan
  sebabnya (biasanya port terpakai, yang juga dilaporkan Overview dengan tawaran pindah port).
- **`*.test` tidak dikenali**: Sites → **Sync** memperbarui berkas hosts (butuh elevasi sekali).
- **Browser memperingatkan sertifikat**: Settings → **Trust local CA**, lalu buka ulang browser.
  Di Linux perlu `certutil` (`sudo apt install libnss3-tools` atau `dnf install nss-tools`) untuk
  Chrome, Brave, dan Firefox. Firefox di Windows/macOS membaca sertifikat sistem hanya jika
  `security.enterprise_roots.enabled` diset `true` di `about:config`, atau impor `etc/ssl/ca.crt`
  secara manual. Alat localhost (Adminer, phpMyAdmin) dibuka lewat http biasa dan tidak butuh ini.
- **Peringatan "Unknown publisher" / Gatekeeper saat memasang**: installer belum ditandatangani;
  lihat [SIGNING.md](SIGNING.md).
- **"Server key changed" saat SSH**: server dipasang ulang, atau ada yang menyadap koneksi.
  Pastikan ke admin sebelum mempercayai kunci baru.
- **Diminta kata sandi padahal sudah disimpan**: penyimpanan kredensial sistem menolak atau tidak
  menjawab; Veranda beralih ke berkas privat dan memberi tahu di halaman Keychain.
- **Linux: jendela hitam atau berkedip**: Settings → matikan **GPU rendering**, buka ulang
  Veranda.
