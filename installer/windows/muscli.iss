#define MyAppName "muscli"
#ifndef MyAppVersion
  #define MyAppVersion "0.2.0-beta.2"
#endif
#ifndef PayloadDir
  #define PayloadDir "..\..\dist\windows"
#endif

[Setup]
AppId={{EE31BC3E-257A-4C1E-A454-7192F3672F7A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=Kumpelo
AppPublisherURL=https://github.com/Kumpelo/muscli
AppSupportURL=https://github.com/Kumpelo/muscli/issues
DefaultDirName={localappdata}\Programs\muscli
DefaultGroupName=muscli
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\..\dist
OutputBaseFilename=muscli-v{#MyAppVersion}-windows-x86_64-setup
UninstallDisplayIcon={app}\muscli.exe
LicenseFile=..\..\LICENSE
WizardStyle=modern
ChangesEnvironment=no

[Files]
Source: "{#PayloadDir}\muscli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\mpv.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\muscli"; Filename: "{app}\muscli.exe"; WorkingDir: "{userdocs}"
Name: "{group}\muscli Doctor"; Filename: "{app}\muscli.exe"; Parameters: "doctor"; WorkingDir: "{userdocs}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\muscli.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\muscli.exe"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\muscli.exe"; ValueType: string; ValueName: "Path"; ValueData: "{app}"

[Run]
Filename: "{app}\muscli.exe"; Description: "Launch muscli"; Flags: nowait postinstall skipifsilent
