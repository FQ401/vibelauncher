# VibeLauncher interface texts. English is the source language.
# Other languages are plain .ftl files with the same message ids.

nav-home = Home
nav-instances = Instances
nav-catalog = Catalog
nav-account = Account
nav-settings = Settings
nav-tasks = Tasks

action-play = Play
action-stop = Stop
action-configure = Configure
action-cancel = Cancel

state-preparing = Preparing
state-preparing-percent = Preparing { $percent }%
state-playing = Playing
state-problem = Problem: { $problem }

home-other-instances = Other instances
home-play-time = Play time: { $time }
instances-empty = No instances yet
tasks-empty = No active tasks
mock-data = Test data
backend-lost = Core is not responding
last-error = Something went wrong
dismiss-hint = Click to dismiss
startup-failed = The launcher could not start
theme-to-dark = Dark theme
theme-to-light = Light theme

col-name = Name
col-version = Version
col-loader = Loader
col-played = Play time

time-minutes = { $minutes } min
time-hours-minutes = { $hours } h { $minutes } min

soon = This section is coming later
stub-catalog = Search for mods, modpacks, resource packs and shaders.
stub-account = Microsoft accounts and offline names.
stub-settings = Launcher settings: Java, memory, appearance.

task-launch = Launching the game
task-install-version = Installing a version
task-download = Downloading files
task-refresh-versions = Refreshing the version list
task-sign-in = Signing in
task-java-scan = Looking for Java
task-java-download = Downloading Java
task-install-resource = Installing files
task-install-pack = Installing a modpack
task-update-pack = Updating a modpack
task-other = Task
task-with-subject = { $task }: { $subject }

## Crash diagnosis (messages and suggested actions)
crash-wrong-java = The game needs Java { $java_major }.
crash-corrupt-files = Game files are damaged or incomplete.
crash-native-memory = The system could not give the game the memory it asked for.
crash-out-of-memory = The game ran out of memory.
crash-graphics-driver = The graphics driver crashed the game ({ $frame }).
crash-jvm-crash = The Java virtual machine crashed ({ $frame }).
crash-wrong-game-version = { $mod } does not support this Minecraft version (needs { $expected }, found { $found }).
crash-wrong-loader-version = { $mod } does not support this mod loader version (needs { $expected }, found { $found }).
crash-wrong-dependency-version = { $mod } needs { $dependency } { $expected }, but { $found } is installed.
crash-missing-dependency = { $mod } needs { $dependency }, which is not installed.
crash-duplicate-mod = The same mod is installed more than once ({ $mod }).
crash-entrypoint-failure = The mod loader reports that { $mod } failed to start.
crash-loader-error = The mod loader reports a problem with the installed mods.
crash-mixin-conflict = A mod failed to apply its changes to the game; mods may conflict.
crash-suspect-single = The crash possibly involves one mod.
crash-suspect-several = The crash involves several mods; the cause is not clear.
crash-undetermined = The cause of the crash could not be determined.

crash-action-select-java = Choose a suitable Java
crash-action-verify-files = Check and repair the game files
crash-action-lower-memory = Lower the memory limit
crash-action-raise-memory = Raise the memory limit
crash-action-update-driver = Update the graphics driver
crash-action-export-report = Export the crash report
crash-action-change-game-version = Change the game version
crash-action-change-loader-version = Change the mod loader version
crash-action-update-dependency = Update { $dependency }
crash-action-install-dependency = Install { $dependency }
crash-action-remove-duplicate = Remove the duplicate mod
crash-action-review-loader-message = Read the mod loader message
crash-action-disable-mod = Disable the mod
crash-action-find-culprit = Find the mod that causes the crash
