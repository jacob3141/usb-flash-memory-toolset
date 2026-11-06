; SPDX-License-Identifier: GPL-3.0-or-later
;
; Inno Setup script of the Windows installer. Build it with scripts/build-installer.ps1.

#ifndef AppVersion
  #error Pass the version: ISCC /DAppVersion=1.2.3
#endif

#define AppName "USB Flash Memory Toolset"
#define AppExe "usbflash-gui.exe"
#define RepoRoot "..\.."

[Setup]
; Identifies the product across versions. Never change it, or upgrades break.
AppId={{6F1D2C54-8A3B-4E07-9C61-2B7E5D90A1F3}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Jacob Dawid and contributors
AppPublisherURL=https://github.com/jacob3141/usb-flash-memory-toolset
AppSupportURL=https://github.com/jacob3141/usb-flash-memory-toolset/issues
AppUpdatesURL=https://github.com/jacob3141/usb-flash-memory-toolset/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
LicenseFile={#RepoRoot}\LICENSE
; Raw disk access needs administrator rights anyway.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#RepoRoot}\target\installer
OutputBaseFilename=usbflash-toolset-{#AppVersion}-windows-x64-setup
SetupIconFile={#RepoRoot}\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
WizardImageFile={#RepoRoot}\assets\installer\wizard-100.bmp,{#RepoRoot}\assets\installer\wizard-150.bmp,{#RepoRoot}\assets\installer\wizard-250.bmp
WizardSmallImageFile={#RepoRoot}\assets\installer\small-100.bmp,{#RepoRoot}\assets\installer\small-150.bmp,{#RepoRoot}\assets\installer\small-250.bmp
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"

[CustomMessages]
english.AddToPath=Add the &usbflash command line tool to the PATH
german.AddToPath=&usbflash-Kommandozeilenwerkzeug in den PATH aufnehmen

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "{cm:AddToPath}"; Flags: unchecked

[Files]
Source: "{#RepoRoot}\target\release\usbflash-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\target\release\usbflash.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\SAFETY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\THIRD_PARTY_LICENSES.html"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvironmentKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';

function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKLM, EnvironmentKey, 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

// Removes exactly our own entry and leaves the rest of the PATH untouched.
procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKLM, EnvironmentKey, 'Path', Paths) then
    exit;
  Paths := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
  if P = 0 then
    exit;
  Delete(Paths, P, Length(Dir) + 1);
  Paths := Copy(Paths, 2, Length(Paths) - 2);
  RegWriteExpandStringValue(HKLM, EnvironmentKey, 'Path', Paths);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
