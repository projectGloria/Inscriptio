; Inscriptio installer (Inno Setup 6.7+). Build with installer\build.ps1, which
; passes AppVersion from Cargo.toml so there's exactly one version number to bump.
;
; Per-user install: no UAC prompt, nothing outside the user's profile. That
; matches the app, which only ever touches HKCU and %LOCALAPPDATA%.

#ifndef AppVersion
  #error Run installer\build.ps1 (it defines AppVersion from Cargo.toml)
#endif

#define AppName "Inscriptio"
#define AppExe  "inscriptio.exe"
; The hidden main window's class (src/main.rs). Used to close the running copy politely.
#define MainClass "InscriptioWnd"

[Setup]
; AppId identifies the install for upgrades and uninstall. Never change it.
AppId={{6F2B8E4A-3C1D-4E7B-9A5F-1D2C3B4A5E6F}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=projectGloria
AppPublisherURL=https://github.com/projectGloria/Inscriptio
VersionInfoVersion={#AppVersion}
; lowest = per-user: {autopf} resolves to %LOCALAPPDATA%\Programs.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; GetDpiForWindow / per-monitor-v2 DPI need Windows 10 1703+.
MinVersion=10.0.15063
SetupIconFile=..\Inscriptio.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern dark hidebevels
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=Inscriptio-{#AppVersion}-setup
; We close the app ourselves in PrepareToInstall (see [Code]); Restart Manager
; can't, because a tray app has no visible window to ask.
CloseApplications=no

[Tasks]
Name: autostart; Description: "Start Inscriptio with Windows"; GroupDescription: "Options:"
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; --settings: a second launch hands the request to the running copy (single
; instance), so the shortcut always "opens Inscriptio", whether or not it's running.
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Parameters: "--settings"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Parameters: "--settings"; Tasks: desktopicon

[Registry]
; Same value name and quoting as settings::set_autostart(), so the in-app
; "Start with Windows" switch and this checkbox are the same setting.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; Tasks: autostart
; Settings and stats. Removed on uninstall: no leftovers.
Root: HKCU; Subkey: "Software\{#AppName}"; Flags: uninsdeletekey

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--settings"; Description: "Launch Inscriptio"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
; Trim history lives here (src/history.rs). It's a list of links you copied:
; uninstalling must not leave it behind.
Type: filesandordirs; Name: "{localappdata}\{#AppName}"

[Code]
const
  WM_CLOSE = $0010;

{ Ask the running copy to exit cleanly: WM_CLOSE -> WM_DESTROY removes the tray
  icon and the clipboard listener. taskkill /F would leave a ghost tray icon
  until you hover over it, so it's only the fallback after 3 s. }
procedure CloseRunningApp;
var
  Wnd: HWND;
  I, Rc: Integer;
begin
  Wnd := FindWindowByClassName('{#MainClass}');
  if Wnd = 0 then Exit;
  PostMessage(Wnd, WM_CLOSE, 0, 0);
  for I := 1 to 30 do
  begin
    Sleep(100);
    if FindWindowByClassName('{#MainClass}') = 0 then Exit;
  end;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Rc);
  Sleep(300); { give the OS a moment to release the exe's file lock }
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseRunningApp; { upgrades: the exe can't be overwritten while it runs }
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseRunningApp;
    { Not a [Registry] uninsdeletevalue: the user may have turned autostart on
      in the app later, after unticking it here. Delete it unconditionally. }
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
  end;
end;
