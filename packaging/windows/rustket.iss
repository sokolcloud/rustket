; Inno Setup 6.3+ script for Rustket. Built by packaging/windows/build.ps1,
; which passes:
;   /DAppVersion=0.1.0 /DAppNumericVersion=0.1.0.0 /DAppArch=x64|arm64
;   /DSourceDir=<dir with rustket.exe, LICENSE.txt, THIRD-PARTY-NOTICES.txt>
;   /DOutputDir=<dir>
; Output: Rustket-<version>-<arch>-setup.exe. Per-user install by default
; (no UAC prompt); the first page offers an all-users install.

#ifndef AppVersion
  #error Define AppVersion (/DAppVersion=...)
#endif
#ifndef AppNumericVersion
  #error Define AppNumericVersion (/DAppNumericVersion=x.y.z.0)
#endif
#ifndef AppArch
  #error Define AppArch (/DAppArch=x64 or arm64)
#endif
#ifndef SourceDir
  #error Define SourceDir (/DSourceDir=...)
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

; Must match the app (AppUserModelID and the rocketchat:// registration it
; writes itself; see the Windows desktop integration code).
#define AppUserModelID "io.github.sokolcloud.rustket"
#define Scheme "rocketchat"

[Setup]
; Never change AppId: upgrades and uninstall find the installation by it.
AppId={{77810904-1236-4291-B123-CCC8D0A6CCF1}
AppName=Rustket
AppVersion={#AppVersion}
AppVerName=Rustket {#AppVersion}
AppPublisher=Sergey Sokolov
AppPublisherURL=https://github.com/sokolcloud/rustket
AppSupportURL=https://github.com/sokolcloud/rustket/issues
AppUpdatesURL=https://github.com/sokolcloud/rustket/releases
VersionInfoVersion={#AppNumericVersion}
VersionInfoProductName=Rustket
VersionInfoDescription=Rustket Setup
DefaultDirName={autopf}\Rustket
DefaultGroupName=Rustket
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
#if AppArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#elif AppArch == "x64"
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#else
  #error AppArch must be x64 or arm64
#endif
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=Rustket-{#AppVersion}-{#AppArch}-setup
SetupIconFile={#SourcePath}..\..\crates\rustket\assets\icons\rustket.ico
UninstallDisplayIcon={app}\rustket.exe
UninstallDisplayName=Rustket
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Restart Manager closes a running rustket.exe that is about to be replaced
; (the [Code] check below asks first in interactive installs).
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\rustket.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Rustket"; Filename: "{app}\rustket.exe"; AppUserModelID: "{#AppUserModelID}"
Name: "{autodesktop}\Rustket"; Filename: "{app}\rustket.exe"; AppUserModelID: "{#AppUserModelID}"; Tasks: desktopicon

[Registry]
; Byte-identical to what the app writes for itself. Removed by [Code] on
; uninstall only while it still points at this installation.
Root: HKA; Subkey: "Software\Classes\{#Scheme}"; ValueType: string; ValueName: ""; ValueData: "URL:Rocket.Chat Protocol"
Root: HKA; Subkey: "Software\Classes\{#Scheme}"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\{#Scheme}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\rustket.exe"",0"
Root: HKA; Subkey: "Software\Classes\{#Scheme}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\rustket.exe"" ""%1"""

[Run]
Filename: "{app}\rustket.exe"; Description: "{cm:LaunchProgram,Rustket}"; Flags: nowait postinstall skipifsilent

[Code]
function ExePath(): String;
begin
  Result := ExpandConstant('{app}\rustket.exe');
end;

{ True while a rustket.exe started from this installation runs. }
function IsRustketRunning(): Boolean;
var
  Locator, Service, Processes: Variant;
  Path: String;
begin
  Result := False;
  Path := ExePath();
  StringChangeEx(Path, '\', '\\', True);
  StringChangeEx(Path, '''', '\''', True);
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Processes := Service.ExecQuery(
      'SELECT ProcessId FROM Win32_Process WHERE ExecutablePath = ''' + Path + '''');
    Result := Processes.Count > 0;
  except
    Log('Could not query running processes: ' + GetExceptionMessage);
  end;
end;

{ Asks the user to close Rustket; False when they cancel. }
function WaitUntilClosed(Message: String): Boolean;
begin
  Result := True;
  while IsRustketRunning() do
    if MsgBox(FmtMessage(Message, ['Rustket']), mbError, MB_OKCANCEL) = IDCANCEL then
    begin
      Result := False;
      Exit;
    end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  { Silent installs rely on Restart Manager (CloseApplications). }
  if not WizardSilent() and not WaitUntilClosed(SetupMessage(msgSetupAppRunningError)) then
    Result := FmtMessage(SetupMessage(msgSetupAppRunningError), ['Rustket']);
end;

function InitializeUninstall(): Boolean;
begin
  Result := UninstallSilent() or WaitUntilClosed(SetupMessage(msgUninstallAppRunningError));
end;

{ Deletes the scheme registration under RootKey if it starts this installation. }
procedure RemoveScheme(RootKey: Integer);
var
  Command: String;
begin
  if RegQueryStringValue(RootKey, 'Software\Classes\{#Scheme}\shell\open\command', '', Command)
    and (CompareText(Command, '"' + ExePath() + '" "%1"') = 0) then
    RegDeleteKeyIncludingSubkeys(RootKey, 'Software\Classes\{#Scheme}');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Value: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  { The installer wrote HKA; the app itself may also have written HKCU. }
  RemoveScheme(HKA);
  if IsAdminInstallMode() then
    RemoveScheme(HKCU);
  { Autostart entry written by the app's settings, if it starts this copy. }
  if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Rustket', Value)
    and (Pos(Lowercase(ExePath()), Lowercase(Value)) > 0) then
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Rustket');
  { Toast registration; the app recreates it on start (also the portable zip). }
  RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\AppUserModelId\{#AppUserModelID}');
end;
