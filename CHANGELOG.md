# Changelog

Every released change, newest first. The product page keeps only the latest.

## 0.4.4

- **Proprietary licence.** Kaelis is now freeware: free to download and use, not
  to redistribute or modify. The MIT licence no longer applies to new builds;
  0.4.3 and earlier keep the terms they shipped with.
- **The new terms ship inside the packages.** The portable ZIP, the installer,
  the portable guide, and the published licence page all carry the Kaelis
  licence instead of the MIT text.
- **A product README.** One line per feature instead of paragraphs. The full
  history lives in this file, and build and maintenance notes moved to
  `DEVELOPING.md`.
- **Compact site copy.** Shorter feature cards, FAQ answers, and metadata; no
  open-source claims.

## 0.4.3

- **Back in the taskbar.** The forced `WS_EX_TOOLWINDOW` rewrite is gone, so a
  normal launch keeps a taskbar button and Alt+Tab entry again. The window also
  carries a real title (`Kaelis`), which was empty before, so the shell has a
  name for it instead of the raw process name. Minimize is an ordinary minimize
  on every platform; the tray icon remains, and its **Hide** is the explicit way
  to put the window away. Deleting the style hook also removes an unsafe window
  subclass and its retry path from every frame.
- **The game stays at high priority.** A launched JVM is promoted once, right
  after spawn, and is never demoted. The foreground hook, its message-loop
  thread, the idle demotion, and the EcoQoS throttle on the game are all gone,
  so alt-tabbing no longer drops the world loading in the background to idle.
  `REALTIME_PRIORITY_CLASS` is still not used: it needs elevation and can starve
  input and audio. High is the highest class that is both reachable and safe.
- **One version everywhere.** Every crate now inherits the workspace version
  instead of carrying its own. 0.4.2 shipped a launcher that reported itself as
  0.4.2 in the tray, About card, update check, and report metadata; 0.4.3's
  single value cannot drift again. The release and link scripts read that one
  value.
- **Lighter.** The shipped Windows executable is **17,232,896 B** (**16.4 MB**),
  4,608 B smaller than 0.4.2's, with the same icon and both renderers.
- **Tests:** 282, with `cargo fmt --check` and Clippy `-D warnings` clean.

## 0.4.2

- **The taskbar is really gone this time.** 0.4.1 wrote `WS_EX_TOOLWINDOW`
  once, and both winit and Explorer recompute the style when the window is
  shown, so the button came back. The window procedure now edits every
  `WM_STYLECHANGING` for `GWL_EXSTYLE`, keeping `WS_EX_TOOLWINDOW` and dropping
  `WS_EX_APPWINDOW`, so no later style write can restore the button. The
  viewport also asks the toolkit for a taskbar-less window
  (`ViewportBuilder::with_taskbar(false)`), which deletes the tab at creation
  and after an Explorer restart. Verified against the running release: the
  visible window has `WS_EX_TOOLWINDOW`, no `WS_EX_APPWINDOW`, and the shell's
  taskbar exposes no Kaelis item — before and after a hide/show cycle.
- **Everything from 0.4.1.**

## 0.4.1

- **The window bar is only a handle.** The selected channel's name is gone from
  the strip: the rail already shows it and the page below repeats it. The bar
  keeps the drag surface and the three window controls.
- **The game in front runs at high priority.** A launched JVM is promoted to
  `HIGH_PRIORITY_CLASS` while it loads and while it owns the foreground, and is
  demoted to `IDLE_PRIORITY_CLASS` only after it has been in front once and the
  user switches away. A JVM loading mods and a world is therefore never
  throttled before its window appears, and a background instance still gives up
  the fast cores through EcoQoS. A Windows test spawns a child process and
  asserts both classes. Unix does not let an unprivileged process raise another
  process's priority, so there is no equivalent switch to flip there.
- **Parallel reference processing.** The default flags add
  `-XX:+ParallelRefProcEnabled` beside the heap and G1 settings, so
  weak-reference cleanup uses every collector worker instead of one. No region
  size, pause target, or thread count is pinned.
- **The icon is back.** The executable embeds `assets/kaelis.ico` again and the
  window sets the same 64 px mark, so Explorer, dialogs, and the tray menu show
  the brand instead of a generic executable. The ICO costs 46,732 B; the
  rebuilt executable is **18,357,760 B**, the same size as 0.4.0's.
- **Scope, stated plainly:** the launcher can give a game the highest practical
  scheduling priority and tune its JVM, but it cannot overclock a CPU or GPU and
  it cannot make Minecraft's single render thread multi-core. No timer that
  protects disk, network, or correctness was removed.
- **Tests:** 283 (the window-icon test returns with the icon, and the taskbar
  style rewrite is asserted as a pure function).

## 0.4.0

- **An idle launcher is off the schedule.** A hidden or unfocused Kaelis asks
  Windows for background processing mode and EcoQoS, so its threads sit at the
  bottom of the CPU priority, its disk and memory priorities are at their
  minimums, and the scheduler is free to run it on efficiency cores at a low
  clock. With the lowest memory priority the OS also pages it out: the working
  set falls from ~81 MB to ~32 MB, and to ~19 MB after a tray wake.
- **A hidden launcher no longer burns a core.** Hiding the window left the
  toolkit waiting for a paint an invisible window can never deliver, so its
  event loop spun at 5.13 % of a core in a 60-second hidden sample. The
  window is now minimized before it is hidden — which is the state the toolkit
  checks before scheduling another paint — so a hidden launcher costs 0 ms of
  CPU in every 2-second window after start-up, with no frames and no timers.
- **Updates install instantly, without a console.** The verified download is
  renamed into place *inside* the running launcher (Windows permits renaming a
  running image), and the new build starts with the pid it must wait for. The
  batch script, its one-second poll, and the black console window a restarted
  build used to flash are all gone from the normal path; the script remains only
  as the fallback for an install that refuses the rename.
- **The instance governor is event-driven.** Background instances were demoted
  by a timer that woke every 1.5 s and could be up to 1.5 s late; a foreground
  hook now demotes the moment the user switches away and promotes the moment
  they come back, and a demoted instance also gives up the fast cores through
  EcoQoS. Between switches the governor thread blocks in the message queue and
  costs nothing.
- **`--replace-pid`** is the internal hand-off the updater uses: it waits on the
  replaced process's handle rather than polling for its pid to disappear.
- **Dependencies.** `windows-sys` was unified on 0.61 across all four crates
  (two versions were being linked before).
- **Tests:** 281 passing (was 279), including the in-place swap and the backup
  cleanup that the instant updater depends on.

### From 0.3.7

- **No keyboard shortcuts.** `Ctrl/Cmd + 1…5`, `Ctrl + F`, and `Ctrl + S` are
  gone together with `tab_from_shortcuts` and the shortcut labels in the rail
  tooltips, the Instances count line, the Settings scale row, and the item
  tooltips. Nothing is consumed before the widgets see it. Standard text
  editing is untouched: Tab still moves focus, Escape still dismisses a preview,
  and the clipboard still pastes into a field.
- **Six captions and nine decorative labels removed.** The window bar carries
  only the tab name and three controls — no `PLAYING` pill and no status
  sentence; `ACTIVE INSTANCE` and its `Manage content` button are gone from
  Overview; `Your instances` is gone from Instances, which now starts with its
  filter row; `BROWSE`, `INSTANCE`, and the `Loader` labels are gone from
  Content, along with the `MC 1.21.1` and `fabric` chips that repeated what the
  instance box already said, the `50 of 50` counter, the `Add files…` button
  beside Folders, and the `Detected fabric` pill that repeated the combo box.
- **Helper sentences deleted.** `Name it to begin.`, `Pick a Minecraft version.`,
  `Java installs on first play.`, `Name it above, pick a version, and Play
  provisions Java and the client.`, the `Mods, packs, shaders, and worlds stay
  together.` line, and the whole **Zero-setup services** card are gone. The
  Create button refuses and explains itself in its own tooltip instead.
- **Copy:** the tagline is **Minecraft Launcher** (app, README, site title, and
  hero), the sidebar credits **Rust** instead of `Rust 2024`, and both create
  surfaces say **Create instance**.

### From 0.3.6

- **The Content screen has no boxes left to read.** The All / Installed / Not
  installed filter row is gone, and with it the two pills that sat beside every
  result title: an `Install` action already means the project is absent and an
  `Erase` already means it is present, so the pills repeated the button. The
  provider is still in the Details preview, where a single project is being
  examined. The filter enum and its plumbing are deleted rather than hidden, so
  nothing dead is left behind, and each row now lays out two fewer widgets.
- **Search fields are capped, not stretched.** The mod search, the results
  refine, the installed filter, and the instance filter stop at 420/320/300/330
  points instead of filling whatever width the window has, and the sort control
  shares the refine row. On a 1440 px display that is most of a row of empty
  surface removed from three bars.
- **Every text button is tighter.** Default padding drops from 12×4 to 9×3, and
  the fixed Install width from 96 to 78 points (the longest state label plus the
  compact padding), so Create, Play, Install, Erase, Copy, and Search all lose
  the same dead margin without changing the 26 pt control height or the row
  alignment.
- **The window bar is a handle and a title.** Header padding drops from 7 to 4
  points and the tab name from 14 to 12.5; the status sentence moved to the
  footer, where it truncates instead of pushing the FPS readout and the full
  text stays one hover away.

### From 0.3.5

- **The window can be made short again.** The minimum height drops from 520 to
  320 points, and the restore check reads the same constant, so a window you
  resize small reopens small instead of snapping back to 1080×700. The floor
  is what the chrome actually needs: header, footer, margins, and the 220 pt
  body floor the Content browser sets for its panes. Everything below that
  scrolls. Read back from the running window, the drag limit is 760×320.
- **The sidebar stops overlapping itself.** The identity card is replaced by the
  30 px avatar button when the tab list leaves less than 88 pt below it, so a
  short window still shows all five tabs. Signed out, that disc reads `K`
  instead of the `N` it was taking from the words "No account".
- **Two waits removed.** A typing pause in the Content browser now starts its
  search after 200 ms instead of 450 ms, and on macOS and Linux the launcher
  notices a finished game every 100 ms instead of every 300 ms (Windows was
  already event-driven). Neither changes what is sent or how often a game is
  polled while it runs.

### From 0.3.4

- **Smoothly rounded corners, from the compositor.** The window asks Windows 11
  for rounded corners and a border-less edge, so the rounding happens at
  composition time and costs the launcher nothing per frame: no transparent
  surface, no per-pixel alpha, no extra draw. Windows 10 ignores the request and
  keeps its own default, as do macOS and Linux.
- **The window controls are drawn as strokes.** Minimize, maximize or restore,
  and close are painted lines in the system's own shapes instead of font
  glyphs, so they stay crisp at every scale factor and no longer depend on the
  icons a font happens to carry. Close keeps the system's red hover.
- **Every drop shadow is gone.** Cards, menus, and popups are separated by flat
  surfaces and hairlines only: fewer feathered shapes to tessellate, and no
  pixels lit in a dark room that an OLED panel would otherwise switch off.
- **The window surface is opaque.** The launcher paints every pixel it owns, so
  requesting per-pixel transparency bought nothing and cost a full-surface
  alpha blend on every frame. Measured on the development host, window time and
  working set move within host noise either way; what changed is that work was
  removed rather than added.

### From 0.3.3

- **Every component sits at its newest patch release.** `openssl-probe` 0.2.2,
  `roxmltree` 0.21.2, and `rustix` 1.1.6 replace their previous patch versions.
  No behaviour changes were needed; the launcher is built, linted, and tested
  against the newest versions crates.io offers inside the ranges the project
  already allows.

### From 0.3.2

- **The launcher remembers how it was left.** The window reopens at the size it
  was last settled on, and the switches reopen where they were set: version-list
  snapshots, alpha and beta versions, required dependencies, the search
  providers, and search-as-you-type. A maximized window is never recorded, so
  restoring never pins the launcher to a full screen, and a corrupt or
  implausible saved size falls back to the default that fits a 1366×768 laptop.
- **The window is a frameless overlay.** There is no native title bar: the
  launcher's own header is the handle. Drag anywhere on the strip to move the
  window, double-click it to maximize or restore, and use one control each to
  minimize, maximize or restore, and close. Close ends the process immediately,
  with the current geometry written first. On Windows minimize still hides to
  the tray. Resizing works from all eight edges and corners, with the cursor
  changing over each grip.
- **Shorter copy everywhere.** Descriptions on Instances, Content, and Settings
  are one line each and say the same thing they did before.

### From 0.3.1

- **Play repairs the instance before it launches.** Missing pieces are found
  with filesystem checks only — no network round trip — and then filled from
  Mojang's own metadata: the version-matched Java runtime, the version
  metadata, the client jar, any missing library, the asset index, missing game
  assets, and a Fabric-family loader. The launch continues by itself once the
  gap is closed, so a fresh machine, a half-finished download, or a deleted
  library is no longer a dead end. A gap Kaelis cannot fill on its own — a
  Forge or NeoForge loader — is reported with the reason instead of failing
  deep inside the game's log.
- **Minecraft's own telemetry is forced off.** Every launch rewrites the
  instance's `options.txt` to `telemetryOptIn:false` (and the legacy
  `snooperEnabled`) while preserving every other line and its order. Kaelis
  itself still collects nothing.
- **A configured Java that moved no longer fails a launch.** The configured
  path is tried first; when it no longer resolves, the instance runtime, the
  `PATH`, and then an installed JDK are used, and only a machine with none of
  those reports the problem by name.
- **Quieter idle timers.** The temp sweep runs every 15 minutes over an hour of
  grace instead of every 5, and a completed install writes a small marker so a
  healthy instance starts without walking its asset tree.

### From 0.3.0

- **Every platform has an installable build.** Windows keeps the portable ZIP
  and per-user setup. macOS now ships `Kaelis-macos-universal.dmg`, one image
  that holds the universal app and an Applications shortcut. Linux now ships
  `Kaelis-linux-x64.deb` for `apt install ./…` and
  `Kaelis-linux-x64.AppImage` for a single portable file; the tarball remains
  for a plain binary and desktop entry.
- **One tag builds, hashes, and serves all of them.** The release workflow
  produces every artifact from the same commit, writes one combined
  `SHA256SUMS.txt`, and the Pages deploy fetches all platforms before hydrating
  the page. The download cards show real sizes and hashes, and CI fails if any
  artifact link stops resolving.
- **No new runtime or daemon.** The DMG and `.deb` carry the same binary the
  tarball does. The AppImage adds only its `AppRun` wrapper and icon; it uses the
  desktop's GTK and app-indicator libraries instead of duplicating them, so the
  package stays the smallest form that still installs cleanly.

### From 0.2.12

- **Tray minimize and junk cleanup are no longer optional.** Both are core
  Kaelis behaviour, so Settings no longer offers a switch that can turn either
  one off. On Windows, minimize always hides to the tray; junk is erased at
  every start and after every game on every platform. A saved `false` in an
  older settings file is ignored, so an old file cannot re-enable the clutter.
- **“Erase” sits in the middle of its capsule.** The content action row used a
  top-aligned layout, which pushed the label above the button's centre while
  **Install** sat centred. The row now centres every action, and the destructive
  button's label is exactly 12 px from each side.
- **Shorter Settings copy.** The startup entry stands alone under **Startup**,
  automation labels fit one line, and the service note is one sentence. The
  retired window and cleanup switches leave no empty card behind.

### From 0.2.11

- **The instance card no longer says “Never played”.** An instance that has
  never been started shows only what it contains: its mod, pack, shader, and
  world counts. A played instance still shows its relative session time, and
  the counts read naturally — `1 pack · 1 world`, never `1 packs` or `1 worlds`.
- **Auto Erase Junk reaches ReplayMod and Forge logs.** Disposable cleanup now
  also clears the ReplayMod frame cache (recordings themselves stay), the
  `fml-client-latest.log` and `fml-server-latest.log` files Forge and NeoForge
  leave beside an instance, and Windows' `ehthumbs.db` thumbnail cache. Saves,
  recordings, mods, configs, screenshots, and unknown folders are untouched.
- **One less line of clutter.** The freed chip keeps the content row on a single
  baseline at every card height, so scanning a long instance list is quieter
  and slightly faster to read.

### From 0.2.10

- **Only Windows is offered the install action.** The update check and the
  release link remain everywhere, but macOS and Linux ship as archives that are
  unpacked deliberately, so they no longer show an **Update** label that could
  only answer "already up to date".
- **A failed swap restores the launcher.** If the verified build cannot be
  copied into place — an antivirus lock that outlasts the retry window, a file
  that vanished mid-swap — the script starts the existing executable instead of
  exiting silently, so a failed install never leaves the user with no window.
- **Updates install themselves.** The footer already said when a newer Kaelis
  existed; the version now carries an **Update** action. One click downloads
  the published Windows build, hashes it while it streams in, and refuses it
  unless the SHA-256 matches the checksum in the same release. Only then does
  the launcher write a short swap script, end itself instantly, and restart
  into the new version; a failed copy rolls the old executable back. Downloads
  verify every download, and an install inside a protected folder (such as
  `Program Files`) is reported instead of being half-replaced; the version
  link still opens the release page for those installs and for macOS and Linux.
  A stale staging file is deleted at the next start, so an interrupted attempt
  never accumulates in the temp folder.
- **The tray is dark and one click.** Windows paints tray menus itself, so
  Kaelis now sets the process-wide dark menu mode before the menu exists. The
  menu is **Show / Minimize**, **Play last instance**, one separator, and
  **Quit** — three actions, no submenus. The icon itself toggles the window,
  including a window that is minimised to the taskbar.
- **Minimize means tray.** The title-bar minimize button hides the window and
  trims its working set instead of adding a taskbar button. **Show / Minimize**
  in the tray does the same, and a single click restores the window. 0.2.10
  added a Settings toggle for this; 0.2.12 retired it, so minimize always uses
  the tray.
- **The detected loader is a real control-height pill.** Its height is the same
  as the combo box and every button beside it, with the text on the row's
  centre line, so Content no longer shows a short box floating above a toolbar.
- **Start with Windows, silently.** A Settings toggle adds one per-user
  sign-in entry that opens Kaelis hidden in the tray. It is a plain registry
  value — no service, no scheduled task, no splash, no window over the desktop —
  and it can also be changed with `kaelis --startup on` or `off`.
- **A reopened launcher still knows what is running.** Each game writes its
  process id and creation time when it spawns. On the next start those records
  are checked against the live process table, so the card shows Quit (not Play),
  a second JVM cannot open the same world, and Quit ends the adopted game.
  A normal launch still clears its record when the JVM exits.
- **Auto-clean is broader and safer.** It now also clears OS thumbnail files in
  content folders, the official launcher's account/profile files, `cache`,
  `.architectury-transformer` scratch, and typed-command history. It no longer
  deletes a `tmp…` folder it merely guesses at: saves, mods, configs,
  screenshots, and unknown folders always survive.
- **Content browsing is nearly twice as dense.** A result is now two lines:
  title plus provider/installed state, then author, downloads, loader support,
  and version. Descriptions and categories live in the hover and Details panel.
  The icon is 32 px, gutters are tighter, and the action area no longer overlaps
  long metadata, so a screenful holds roughly twice as many mods.
- **Update checks start themselves.** A background request asks GitHub for the
  newest tag after the first window is up. When it is newer, the footer version
  turns into a single-line link to that release. The check is anonymous, does
  not block the window, stays silent when offline, and never installs anything
  on its own.

### From 0.2.6

- **Quit ends the process now.** The tray's Quit and the title-bar close drop
  the tray icon and exit immediately instead of queuing a frame on a hidden
  window and unwinding every editor, worker, and runtime handle first. Settings
  are already written when they change and game logs are RAM-only, so nothing
  is lost, and no ghost icon is left in the notification area.
- **Play becomes Quit while the game runs.** A running instance's card turns
  its action into a red Quit, and the Overview's **Launch selected** does the
  same for the active instance. One click asks the launch task that already owns
  the JVM to end it — no process scan, no `taskkill` child, no wait for a window
  close — and the card returns to Play when the exit event lands, usually within
  a frame. A stop clicked while Java is still being prepared is kept and applied
  the moment the process exists.
- **Modrinth leads the catalogue.** Modrinth cards are listed before the
  CurseForge mirror in every order: Best match, Most downloaded, and Name A-Z.
  Download totals are not comparable between the two — the mirror counts every
  pack that ever embedded a project — so CurseForge can no longer outrank an
  exact Modrinth match. Duplicate projects still collapse to the Modrinth card.
- **A three-action tray.** Show Kaelis, Play last instance, and Quit, with one
  separator for a single glance. (0.2.7 forces the native menu dark and turns
  the first action into Show / Minimize.)

### From 0.2.5

- **Show Kaelis always appears, even over a running game.** Hiding no longer
  goes through egui's viewport at all: on Windows the real window is hidden with
  Win32, so the toolkit's visible/minimised state can never go stale and leave a
  blank frame behind. Every route — the tray menu, a left click on the tray
  icon, **Launch last instance**, a second launcher, or a Start-menu click —
  runs one native path that shows the window, restores it if minimised, lifts it
  above the game with a topmost round trip, and forces foreground through the
  game's input thread. Measured from a hidden window: back on screen in 119 ms.
- **Smart multi-instance power.** Every JVM Kaelis starts is registered with a
  governor. The game whose window owns the foreground runs at normal priority;
  every other instance drops to the idle priority class within 1.5 s and returns
  to normal the moment it is focused again. Nothing is suspended and no session
  is disconnected — a background instance simply stops competing with the one
  being played.
- **Deeper hibernation, fewer wake-ups.** Starting a game hides the launcher and
  empties its working set immediately; losing focus for 90 s does the same while
  the window stays open. The timer is armed once per focus loss and never
  re-armed, so an idle launcher truly schedules no frames. Restoring faults the
  pages back in on demand. Idle HTTP now keeps at most two sockets for 15 s
  instead of 90.
- **Auto Erase Junk reaches the last traces.** Mod download caches
  (`downloads/`), and the client's typed slash-command history are now removed
  with logs, crash dumps, loader caches, natives, server packs, heap dumps, and
  staging files — at every start, after every game, and from `--clean`.
- **Cleaner control rows.** Create is the one accent action in its row at the
  same height and width as Import/Export, Apply sits at control height, and the
  Overview account and release cards share one bottom line.

### From 0.2.4

- Tray restore first ran from the tray thread, the icon became a left-click
  target, and game-start hibernation used a working-set trim instead of only a
  hidden window.
- The HTTP pool was cut to two idle sockets with a 15 s timeout, and the create
  and apply rows were aligned to one control height.

### From 0.2.3

- **One copy of every game file.** Verified downloads are hard-linked into a
  content-addressed store before the next instance asks for them, so a second
  instance of the same version links the assets, libraries, client jar, and Java
  runtime instead of downloading or storing them again. Entries that no instance
  references are pruned; there is no copy, move, or second decompression. A
  store on another volume falls back to a normal download.
- **Reopening Kaelis always brings it back.** A second launch hands its arguments
  to the running window and exits in about 80 ms; the channel is retried for two
  seconds so a double-click during a cold start still lands, and a native window
  wake covers a lost channel. Clicking the tray icon itself restores the window,
  exactly like the tray action (now **Show / Minimize**), including while a game
  is running.
- **Auto Erase Junk found the rest.** Staging files are removed even when they
  keep their real name (`sodium.jar.part`), and the official launcher's UI state
  and saved Microsoft credential file no longer survive in an instance.
  `kaelis --clean [--json]` runs the whole pass headlessly. (0.2.5 also removed a
  crashed loader's `tmp…` folder by name; 0.2.7 no longer guesses and keeps every
  unfamiliar folder.)

### From 0.2.2

- **CurseForge is a real catalogue now.** Without a key, search reads the public
  mirror of the official v1 API — same paths, JSON, pagination, hashes, and
  dependency metadata as `api.curseforge.com` — so a typed name returns a page of
  projects instead of one exact match, and paging, resource packs, and shaders
  all work. An approved key in `CF_API_KEY` aims the identical calls at the
  official endpoint.
- **The named project ranks first.** Search asks for downloads order, because the
  endpoint's default order ignores relevance (`jei` used to return
  `seamless-ores` first).
- **Dependency graphs come for free.** Required mods install keyless, from the
  same file metadata the official API publishes.
- **Less traffic.** Catalogue pages are cached for a minute, and installs address
  files by their real id, which removes one metadata lookup per install.
- **Degrades, never empties.** If the mirror is unreachable, an exact project
  name, link, or id still resolves through the public index, and the launcher
  says so instead of showing an empty list.
- **A release republishes the site.** Publishing a tag now dispatches the Pages
  deploy, which fetches that release's downloads and hydrates the page, instead
  of waiting for a separate push.

<img width="1080" height="700" alt="Content studio" src="site/assets/app-content.png" />

