; Kalux Windows installer (Inno Setup 6).
;
; Built by .github/workflows/release.yml:
;   ISCC /DAppVersion=0.1.0 /DSourceDir=<abs>\target\release /DOutputDir=<abs>\dist packaging\windows\kalux.iss
;
; Relative paths in this script resolve from this folder, so pass absolute paths on the command line.
;
; Installs per user by default (no admin prompt) into %LOCALAPPDATA%\Programs\Kalux. The "for all
; users" choice on the first page installs into Program Files instead.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\target\release"
#endif

[Setup]
; Never change AppId: Windows uses it to recognise upgrades and the uninstall entry.
AppId={{C43B1ACA-798F-4C62-B89D-EF82C6B30AD3}
AppName=Kalux
AppVersion={#AppVersion}
AppVerName=Kalux {#AppVersion}
AppPublisher=Ignatius Karel
AppPublisherURL=https://github.com/Aitemaa/Kalux
AppSupportURL=https://github.com/Aitemaa/Kalux/issues
AppCopyright=Copyright (c) Ignatius Karel
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Kalux Setup
DefaultDirName={autopf}\Kalux
DefaultGroupName=Kalux
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\..\assets\icon.ico
UninstallDisplayIcon={app}\kalux.exe
UninstallDisplayName=Kalux
WizardStyle=modern
Compression=lzma2/ultra
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=kalux-{#AppVersion}-windows-x64-setup
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "openwith"; Description: "Add Kalux to ""Open with"" for image files"; GroupDescription: "Integration:"

[Files]
Source: "{#SourceDir}\kalux.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
; The Start menu entry is what makes Kalux show up in Windows search.
Name: "{autoprograms}\Kalux"; Filename: "{app}\kalux.exe"; Comment: "Instant native image viewer"
Name: "{autodesktop}\Kalux"; Filename: "{app}\kalux.exe"; Comment: "Instant native image viewer"; Tasks: desktopicon

[Registry]
; "kalux" in the Run dialog (Win+R).
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\kalux.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\kalux.exe"; Flags: uninsdeletekey
; "Open with" entry for supported image files.
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Kalux"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\kalux.exe"",0"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\kalux.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".jpg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".jpeg"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".jfif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".png"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".gif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".webp"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".bmp"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".ico"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".tif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".tiff"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".heic"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".heif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".avif"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\kalux.exe\SupportedTypes"; ValueType: string; ValueName: ".jxl"; ValueData: ""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpg\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jpeg\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jfif\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.png\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.gif\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.webp\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.bmp\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.ico\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.tif\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.tiff\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heic\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.heif\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.avif\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.jxl\OpenWithList\kalux.exe"; ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
