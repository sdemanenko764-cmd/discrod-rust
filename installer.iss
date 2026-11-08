; Установщик Rustcord (Inno Setup 6)
; Собирается автоматически: build-windows.bat или GitHub Actions.

#define AppName "Rustcord"
#define AppVersion "0.7.0"

[Setup]
AppId={{8F3C2A51-7B4E-4C1D-9A2F-5E6B7C8D9E01}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Rustcord
DefaultDirName={localappdata}\Programs\Rustcord
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=RustcordSetup
Compression=lzma2/ultra
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\rustcord.exe
UninstallDisplayName={#AppName}

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "target\release\rustcord.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\rustcord.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\rustcord.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\rustcord.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{userappdata}\Rustcord"
