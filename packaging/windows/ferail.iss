; Inno Setup script for Ferail: driven by scripts/package-win.ps1.
;
; Build directly with:
;   iscc /DSourceDir=..\..\target\package\Ferail /DAppVersion=0.2.1 ferail.iss
;
; The Windows analogue of packaging/macos/Info.plist + scripts/bundle-mac.sh:
; it takes the already-staged payload (exes + licences) and wraps it in a
; per-user-or-per-machine installer with a Start Menu entry and an uninstaller.
;
; Note the deliberate absence of a code-signing step here, signing is
; signtool's job in package-win.ps1, which signs the payload BEFORE this runs
; and the resulting installer AFTER. Inno's SignTool hook would need the cert
; configured in the IDE, which does not survive CI.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\package\Ferail"
#endif

#define AppName        "Ferail"
#define AppPublisher   "John Knipper"
#define AppUrl         "https://github.com/jonx/Ferail"
#define AppExe         "Ferail.exe"
#define CliExe         "ferail.exe"

[Setup]
; Stable across releases, changing it makes Windows treat an upgrade as a
; separate product and leaves the old install behind.
AppId={{EEBE7A1D-2613-4900-8F19-0F955E307518}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#AppVersion}

; `lowest` + overrides-allowed means a normal user installs into
; %LocalAppData%\Programs\Ferail with no UAC prompt, while an admin can elect a
; machine-wide install. {autopf} resolves per that choice. A file manager has no
; reason to demand elevation just to install.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes

; Ferail is 64-bit only.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

OutputDir={#SourceDir}\..
OutputBaseFilename={#AppName}-{#AppVersion}-win-x64-setup
SetupIconFile=..\..\crates\ferail-gpui\resources\ferail.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=yes

; Dual MIT/Apache-2.0: show MIT during setup; both ship in licenses\.
LicenseFile=..\..\LICENSE-MIT

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; Fast NTFS stays a separate, narrowly elevated process. It must be beside
; Ferail.exe in installed builds just as it is in the portable ZIP; the GUI
; deliberately refuses a missing or version-mismatched helper.
Source: "{#SourceDir}\ferail-ntfs-helper.exe"; DestDir: "{app}"; Flags: ignoreversion
; The CLI lives in cli\ because Windows paths are case-insensitive: shipping
; `ferail.exe` beside `Ferail.exe` collapses to a single file. Keep this layout
; in step with scripts/package-win.ps1, which asserts the two are distinct.
Source: "{#SourceDir}\cli\{#CliExe}"; DestDir: "{app}\cli"; Flags: ignoreversion
; MIT/Apache-2.0 (plus the MIT tree-sitter grammars and the ISC/MIT icon
; artwork) require their notices to accompany a redistributed copy: an
; installer carrying only the executables does not satisfy that.
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
