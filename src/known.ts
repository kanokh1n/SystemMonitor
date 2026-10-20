// Пояснения к процессам macOS, у которых непонятные имена.
// Ключ — точное имя процесса. Для остальных пробуем правила ниже.

const KNOWN: Record<string, string> = {
  // Ядро и графика
  kernel_task: "ядро macOS; растёт, когда мак охлаждает процессор",
  WindowServer: "рисует всё на экране: окна, анимации, мониторы",
  launchd: "запускает все остальные процессы",
  loginwindow: "вход в систему и экран блокировки",
  Dock: "Dock, Launchpad и Mission Control",
  Finder: "Finder",
  SystemUIServer: "значки в строке меню",
  ControlCenter: "Пункт управления; слушает порты 5000/7000 для AirPlay",
  NotificationCenter: "Центр уведомлений",

  // Docker и виртуализация
  "com.apple.Virtualization.VirtualMachine": "виртуальная машина Linux, обычно от Docker Desktop",
  "com.docker.backend": "Docker Desktop: основной процесс",
  "com.docker.virtualization": "Docker Desktop: управляет виртуальной машиной",
  "com.docker.build": "Docker Desktop: сборка образов",
  "Docker Desktop": "окно Docker Desktop",
  OrbStack: "OrbStack: Docker и Linux‑машины",
  qemu: "эмулятор виртуальных машин (Colima, Podman)",

  // Поиск и индексация
  mds: "Spotlight: следит за изменениями файлов",
  mds_stores: "Spotlight: хранит поисковый индекс",
  mdworker: "Spotlight: индексирует файлы",
  mdworker_shared: "Spotlight: индексирует файлы",
  corespotlightd: "Spotlight: поиск внутри приложений",
  Spotlight: "окно поиска Spotlight",

  // iCloud и фото
  bird: "iCloud Drive: синхронизация файлов",
  cloudd: "iCloud: синхронизация данных",
  fileproviderd: "iCloud Drive и облачные диски в Finder",
  photoanalysisd: "Фото: распознаёт лица и объекты",
  photolibraryd: "Фото: медиатека",
  mediaanalysisd: "анализ фото и видео (поиск по содержимому)",

  // Сеть и устройства
  mDNSResponder: "DNS и поиск устройств в сети (Bonjour)",
  rapportd: "связь с другими устройствами Apple (Handoff, AirDrop)",
  sharingd: "AirDrop и общий доступ",
  airportd: "Wi‑Fi",
  WiFiAgent: "Wi‑Fi: значок и подключение",
  bluetoothd: "Bluetooth",
  BTLEServer: "Bluetooth Low Energy (часы, AirPods, датчики)",
  nsurlsessiond: "фоновые загрузки приложений",
  apsd: "push‑уведомления Apple",
  identityservicesd: "iMessage и FaceTime: учётные записи",
  configd: "сетевые настройки",
  netbiosd: "сетевые папки Windows (SMB)",

  // Звук, питание, безопасность
  coreaudiod: "звук",
  powerd: "питание и сон",
  hidd: "клавиатура, мышь и трекпад",
  trustd: "проверка сертификатов",
  secd: "Связка ключей",
  syspolicyd: "Gatekeeper: проверка приложений",
  XprotectService: "XProtect: встроенный антивирус",
  softwareupdated: "обновления macOS",
  backupd: "Time Machine: резервная копия",
  logd: "системный журнал",
  ReportCrash: "отчёт о падении программы",
  spindump: "отчёт о зависшей программе",
  cfprefsd: "настройки приложений",
  distnoted: "уведомления между процессами",
  runningboardd: "следит за работой приложений",
  UserEventAgent: "системные события",
  coreduetd: "предсказывает использование приложений",
  knowledgeconstructiond: "Siri и подсказки: изучает привычки",
  siriknowledged: "Siri: база знаний",
  routined: "часто посещаемые места",
  sysmond: "сбор статистики для Мониторинга системы",
  "Activity Monitor": "Мониторинг системы",

  // Разработка
  node: "Node.js",
  rustc: "компилятор Rust",
  cargo: "Cargo: сборка Rust",
  "rust-analyzer": "подсказки Rust в редакторе",
  go: "Go",
  gopls: "подсказки Go в редакторе",
  java: "Java (часто IDE от JetBrains)",
  postgres: "PostgreSQL",
  "redis-server": "Redis",
  nginx: "веб‑сервер nginx",
  "php-fpm": "PHP",
  git: "Git",
  "system-monitor": "это приложение",
  claude: "Claude Code в терминале",

  // Данные пользователя
  contactsd: "Контакты: синхронизация и поиск",
  calaccessd: "Календарь: доступ приложений",
  remindd: "Напоминания",
  CalendarAgent: "Календарь: синхронизация",
  IMDPersistenceAgent: "Сообщения: хранение переписки",
  suggestd: "подсказки в почте, Сообщениях и Spotlight",
};

/** Правила для семейств процессов, у которых меняется хвост имени. */
const RULES: [RegExp, (app: string | null) => string][] = [
  // Chrome называет их «(Renderer)» или «(Aperitif Renderer)»: это страницы, вкладки и расширения.
  [/Helper \((.+ )?Renderer\)$/, (app) => `страница, вкладка или расширение ${app ?? "приложения"}`],
  [/Helper \(GPU\)$/, (app) => `графика ${app ?? "приложения"}`],
  [/Helper \(Plugin\)$/, (app) => `плагины ${app ?? "приложения"}`],
  [/Helper/, (app) => `вспомогательный процесс ${app ?? "приложения"}`],
  [/^com\.apple\.WebKit\.WebContent/, () => "вкладка Safari или веб‑страница в приложении"],
  [/^com\.apple\.WebKit/, () => "движок Safari (WebKit)"],
  [/^mdworker/, () => "Spotlight: индексирует файлы"],
];

/** Что это за процесс, коротко, или null, если сказать нечего. */
export function describe(name: string, app?: string | null): string | null {
  const known = KNOWN[name];
  if (known) return known;
  const rule = RULES.find(([re]) => re.test(name));
  if (rule) return rule[1](app ?? null);
  // Имя процесса не совпадает с приложением: подскажем, чьё оно.
  if (app && app !== name) return `часть ${app}`;
  return null;
}
