//! Linux x86-64 ABI системных вызовов: номера и коды ошибок (M5b).
//!
//! # Почему ровно как в Linux (D8)
//!
//! Номера и семантика копируют **таблицу Linux x86-64** (`arch/x86/entry/syscalls/
//! syscall_64.tbl` в ядре Linux). Это north star проекта (см. `docs/DECISIONS.md` D8):
//! тот же слой потом несёт и source-level POSIX (relibc, M9), и ABI-совместимость с
//! немодифицированными бинарниками — поэтому «правильные» номера сейчас экономят
//! переписывание позже. Реализуем подмножество, но нумерация фиксирована Linux'ом.
//!
//! Коды ошибок — стандартные `errno` (положительные). Соглашение возврата: ядро кладёт
//! в `rax` либо неотрицательный результат, либо `-errno` (диапазон ошибок `[-4095, -1]`).

// --- Номера системных вызовов (Linux x86-64) ---

/// `read(fd, buf, count)`.
pub const SYS_READ: u64 = 0;
/// `write(fd, buf, count)`.
pub const SYS_WRITE: u64 = 1;
/// `open(path, flags, mode)` — открыть файл, вернуть дескриптор.
pub const SYS_OPEN: u64 = 2;
/// `close(fd)`.
pub const SYS_CLOSE: u64 = 3;
/// `readv(fd, iov, iovcnt)` — векторное чтение в массив буферов `struct iovec` (M9f).
pub const SYS_READV: u64 = 19;
/// `writev(fd, iov, iovcnt)` — векторная запись из массива буферов (libc буферизует вывод) (M9f).
pub const SYS_WRITEV: u64 = 20;
/// `fcntl(fd, cmd, arg)` — управление дескриптором (дублирование/флаги) (M9f).
pub const SYS_FCNTL: u64 = 72;
/// `socket(domain, type, protocol)` — создать сокет, вернуть дескриптор (M8d3).
pub const SYS_SOCKET: u64 = 41;
/// `connect(fd, addr, addrlen)` — установить TCP-соединение с удалённым адресом (M8e).
pub const SYS_CONNECT: u64 = 42;
/// `bind(fd, addr, addrlen)` — привязать сокет к локальному адресу/порту (M8d3).
pub const SYS_BIND: u64 = 49;
/// `sendto(fd, buf, len, flags, dest_addr, addrlen)` — отправить датаграмму (M8d3).
pub const SYS_SENDTO: u64 = 44;
/// `recvfrom(fd, buf, len, flags, src_addr, addrlen)` — принять датаграмму (M8d3).
pub const SYS_RECVFROM: u64 = 45;
/// `stat(path, statbuf)` — метаданные файла по пути в `struct stat` (M9c).
pub const SYS_STAT: u64 = 4;
/// `fstat(fd, statbuf)` — метаданные по дескриптору (M9c; для `isatty` и т.п.).
pub const SYS_FSTAT: u64 = 5;
/// `lstat(path, statbuf)` — как `stat`, но без следования по симссылкам; у нас их нет → то же (M9c).
pub const SYS_LSTAT: u64 = 6;
/// `brk(addr)` — задать конец сегмента данных (кучи) процесса (M9a). `addr == 0` (или ниже базы) —
/// запрос текущего разрыва. Возвращает НОВЫЙ разрыв при успехе, СТАРЫЙ — при неудаче (как Linux).
pub const SYS_BRK: u64 = 12;
/// `lseek(fd, offset, whence)` — сдвинуть позицию чтения.
pub const SYS_LSEEK: u64 = 8;
/// `pipe(fds)` — создать канал: `fds[0]` — конец чтения, `fds[1]` — конец записи (M7g2).
pub const SYS_PIPE: u64 = 22;
/// `dup2(oldfd, newfd)` — направить `newfd` на ту же подложку, что и `oldfd` (M7g1; редиректы).
pub const SYS_DUP2: u64 = 33;
/// `getpid()` — идентификатор текущего процесса.
pub const SYS_GETPID: u64 = 39;
/// `getcwd(buf, size)` — текущий рабочий каталог в буфер пользователя (M7c).
pub const SYS_GETCWD: u64 = 79;
/// `chdir(path)` — сменить текущий рабочий каталог (M7c).
pub const SYS_CHDIR: u64 = 80;
/// `mkdir(path, mode)` — создать каталог (M7f; `mode` игнорируем).
pub const SYS_MKDIR: u64 = 83;
/// `rmdir(path)` — удалить пустой каталог (M7g3).
pub const SYS_RMDIR: u64 = 84;
/// `unlink(path)` — удалить файл (M7g3).
pub const SYS_UNLINK: u64 = 87;
/// `fork()` — создать копию процесса (2a3, зарезервировано).
pub const SYS_FORK: u64 = 57;
/// `execve(path, argv, envp)` — заменить образ процесса (2a2, зарезервировано).
pub const SYS_EXECVE: u64 = 59;
/// `exit(status)` — завершить вызывающий поток.
pub const SYS_EXIT: u64 = 60;
/// `wait4(pid, status, options, rusage)` — дождаться завершения потомка (2a4, зарезервировано).
pub const SYS_WAIT4: u64 = 61;
/// `kill(pid, sig)` — послать сигнал процессу (2a5, зарезервировано).
pub const SYS_KILL: u64 = 62;
/// `gettimeofday(tv, tz)` — текущее время в `struct timeval` (сек+мкс), `tz` игнорируем (M9d).
pub const SYS_GETTIMEOFDAY: u64 = 96;
/// `uname(buf)` — информация о системе в `struct utsname` (M9e).
pub const SYS_UNAME: u64 = 63;
/// `getuid()` — реальный UID (у нас всегда 0, root) (M9e).
pub const SYS_GETUID: u64 = 102;
/// `getgid()` — реальный GID (0) (M9e).
pub const SYS_GETGID: u64 = 104;
/// `geteuid()` — эффективный UID (0) (M9e).
pub const SYS_GETEUID: u64 = 107;
/// `getegid()` — эффективный GID (0) (M9e).
pub const SYS_GETEGID: u64 = 108;
/// `getppid()` — PID родителя (M9e).
pub const SYS_GETPPID: u64 = 110;
/// `arch_prctl(code, addr)` — арх-специфичные настройки потока (M9b): у нас — база сегмента FS
/// под TLS (`ARCH_SET_FS`/`ARCH_GET_FS`). libc держит `errno` и thread-local в TLS через FS.
pub const SYS_ARCH_PRCTL: u64 = 158;
/// `time(tloc)` — секунды текущего времени (возврат + запись в `*tloc`, если не NULL) (M9d).
pub const SYS_TIME: u64 = 201;
/// `getdents64(fd, buf, count)` — прочитать записи каталога (для `ls`).
pub const SYS_GETDENTS64: u64 = 217;
/// `clock_gettime(clk_id, tp)` — время часов `clk_id` в `struct timespec` (сек+нс) (M9d).
pub const SYS_CLOCK_GETTIME: u64 = 228;
/// `exit_group(status)` — завершить все потоки процесса (для нас пока то же, что `exit`).
pub const SYS_EXIT_GROUP: u64 = 231;

// --- Команды fcntl(2) (Linux, M9f) ---

/// Дублировать дескриптор в наименьший свободный ≥ `arg`.
pub const F_DUPFD: u64 = 0;
/// Прочитать флаги дескриптора (close-on-exec). У нас не отслеживаются → 0.
pub const F_GETFD: u64 = 1;
/// Установить флаги дескриптора. Игнорируем → 0.
pub const F_SETFD: u64 = 2;
/// Прочитать режим доступа/статус-флаги открытого файла.
pub const F_GETFL: u64 = 3;
/// Установить статус-флаги (например `O_NONBLOCK`). Игнорируем → 0.
pub const F_SETFL: u64 = 4;

/// Максимум элементов `iovec` в одном `readv`/`writev` (Linux `IOV_MAX`).
pub const IOV_MAX: u64 = 1024;

// --- Идентификаторы часов для clock_gettime(2) (Linux, M9d) ---

/// Стенные часы (у нас без RTC = uptime от загрузки).
pub const CLOCK_REALTIME: u64 = 0;
/// Монотонные часы (не идут назад). У нас тот же uptime.
pub const CLOCK_MONOTONIC: u64 = 1;

// --- Тип файла в st_mode для stat(2) (Linux, M9c) ---

/// Маска битов типа файла в `st_mode`.
pub const S_IFMT: u32 = 0o170000;
/// Обычный файл.
pub const S_IFREG: u32 = 0o100000;
/// Каталог.
pub const S_IFDIR: u32 = 0o040000;
/// Символьное устройство (наши stdin/stdout/stderr — консоль/serial).
pub const S_IFCHR: u32 = 0o020000;
/// FIFO/канал (концы `pipe`).
pub const S_IFIFO: u32 = 0o010000;
/// Сокет (M8d3).
pub const S_IFSOCK: u32 = 0o140000;

// --- Подкоманды arch_prctl(2) (Linux x86-64, M9b) ---

/// Установить базу сегмента FS (адрес блока TLS) текущего потока.
pub const ARCH_SET_FS: u64 = 0x1002;
/// Прочитать базу сегмента FS текущего потока в `*addr`.
pub const ARCH_GET_FS: u64 = 0x1003;

// --- Флаги open(2) (Linux x86-64) ---

/// Маска младших бит режима доступа (`O_RDONLY`/`O_WRONLY`/`O_RDWR`).
pub const O_ACCMODE: u64 = 0o3;
/// Открыть только на чтение (0).
pub const O_RDONLY: u64 = 0o0;
/// Открыть только на запись.
pub const O_WRONLY: u64 = 0o1;
/// Открыть на чтение и запись.
pub const O_RDWR: u64 = 0o2;
/// Создать файл, если его нет.
pub const O_CREAT: u64 = 0o100;
/// Обрезать файл до нуля при открытии (если уже существует).
pub const O_TRUNC: u64 = 0o1000;
/// Дописывать в конец: позиция при открытии — в конце файла (M7g1; для `>>`).
pub const O_APPEND: u64 = 0o2000;

// --- Сокеты (Linux x86-64, M8d3) ---

/// Семейство адресов IPv4 (`AF_INET`). `struct sockaddr_in` (16 байт): `sin_family` (u16, порядок
/// хоста), `sin_port` (u16, сетевой порядок), `sin_addr` (4 байта IPv4), `sin_zero[8]`.
pub const AF_INET: u64 = 2;
/// Тип сокета: датаграммы без соединения (`SOCK_DGRAM`, у нас = UDP).
pub const SOCK_DGRAM: u64 = 2;
/// Тип сокета: надёжный поток с установлением соединения (`SOCK_STREAM`, у нас = TCP) (M8e).
pub const SOCK_STREAM: u64 = 1;
/// Размер `struct sockaddr_in` в байтах.
pub const SOCKADDR_IN_LEN: usize = 16;

// --- Номера сигналов (Linux x86-64) ---

/// `SIGKILL` — безусловное завершение (его нельзя перехватить).
pub const SIGKILL: u8 = 9;
/// `SIGSEGV` — некорректное обращение к памяти (наш путь сбоя в кольце 3).
pub const SIGSEGV: u8 = 11;
/// `SIGTERM` — вежливая просьба завершиться (действие по умолчанию — завершение).
pub const SIGTERM: u8 = 15;
/// Наибольший поддерживаемый номер сигнала (стандартные 1..=64).
pub const SIG_MAX: u64 = 64;

// --- Коды ошибок (errno, положительные; в `rax` возвращаются как `-errno`) ---

/// Нет такого файла или каталога.
pub const ENOENT: i64 = 2;
/// Нет такого процесса (для `kill`).
pub const ESRCH: i64 = 3;
/// Объект уже существует (например, `mkdir` существующего каталога, M7f).
pub const EEXIST: i64 = 17;
/// Ошибка ввода-вывода (например, сбой записи на диск).
pub const EIO: i64 = 5;
/// Не каталог (ожидался каталог, например для `getdents64`).
pub const ENOTDIR: i64 = 20;
/// Является каталогом (нельзя `read`/`write` как файл).
pub const EISDIR: i64 = 21;
/// Недопустимый `lseek` (например, по консоли/устройству — оно не позиционируется, M7g1).
pub const ESPIPE: i64 = 29;
/// Запись в канал, у которого не осталось читателей (M7g2; вместо SIGPIPE).
pub const EPIPE: i64 = 32;
/// На устройстве не осталось места (нет свободных кластеров/записи каталога).
pub const ENOSPC: i64 = 28;
/// Неверный формат исполняемого файла (битый/неподдерживаемый ELF).
pub const ENOEXEC: i64 = 8;
/// Слишком длинный список аргументов/окружения (`argv`/`envp` не влезли в стек, M7b).
pub const E2BIG: i64 = 7;
/// Нет потомков (для `wait`).
pub const ECHILD: i64 = 10;
/// Ресурс временно недоступен.
pub const EAGAIN: i64 = 11;
/// Недостаточно памяти.
pub const ENOMEM: i64 = 12;
/// Плохой файловый дескриптор.
pub const EBADF: i64 = 9;
/// Некорректный адрес (указатель вне доступной пользователю памяти).
pub const EFAULT: i64 = 14;
/// Некорректный аргумент.
pub const EINVAL: i64 = 22;
/// Слишком много открытых файлов (исчерпана таблица дескрипторов процесса).
pub const EMFILE: i64 = 24;
/// Системный вызов не реализован.
pub const ENOSYS: i64 = 38;
/// Каталог не пуст (для `rmdir`, M7g3).
pub const ENOTEMPTY: i64 = 39;
/// Результат не помещается в переданный буфер (например, `getcwd` с малым `size`, M7c).
pub const ERANGE: i64 = 34;
/// Операция над не-сокетом (fd указывает не на сокет, M8d3).
pub const ENOTSOCK: i64 = 88;
/// Сообщение слишком длинное (датаграмма не влезла в буфер сокета, M8d3).
pub const EMSGSIZE: i64 = 90;
/// Протокол не поддержан для этого типа сокета (M8d3).
pub const EPROTONOSUPPORT: i64 = 93;
/// Семейство адресов не поддержано (поддержан только `AF_INET`, M8d3).
pub const EAFNOSUPPORT: i64 = 97;
/// Сеть не работает: стек не поднялся (нет NIC / DHCP не настроился, M8d3).
pub const ENETDOWN: i64 = 100;
/// Операция не поддержана для этого типа сокета (например `connect` на UDP, M8e).
pub const EOPNOTSUPP: i64 = 95;
/// Сокет не подключён (`send`/`recv` на неустановленном TCP, M8e).
pub const ENOTCONN: i64 = 107;
/// Истёк таймаут операции (TCP-рукопожатие не завершилось, M8e).
pub const ETIMEDOUT: i64 = 110;
/// Соединение отвергнуто удалённой стороной (RST на `connect`, M8e).
pub const ECONNREFUSED: i64 = 111;
