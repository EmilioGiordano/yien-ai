#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef SourceDir
  #error SourceDir is required
#endif
#ifndef OutputDir
  #error OutputDir is required
#endif
#ifndef OutputBase
  #error OutputBase is required
#endif

[Setup]
AppId={{E5F3AD8A-224E-4A19-83A6-5D8D604B1B81}
AppName=YIEN
AppVersion={#AppVersion}
AppPublisher=YIEN
DefaultDirName={localappdata}\Programs\YIEN
DefaultGroupName=YIEN
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
UninstallDisplayName=YIEN
UninstallDisplayIcon={app}\yien.exe
CloseApplications=yes
CloseApplicationsFilter=yien.exe
RestartApplications=no
SetupMutex=Local\YIENInstaller
WizardStyle=modern
LicenseFile={#SourceDir}\INSTALLER-LICENSE.txt

[Files]
Source: "{#SourceDir}\yien.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\yien-hook.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\conpty.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\OpenConsole.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\INSTALLER-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE*"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\LICENCE*"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\YIEN"; Filename: "{app}\yien.exe"
Name: "{userdesktop}\YIEN"; Filename: "{app}\yien.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if CheckForMutexes('Local\YIENRunning') then
    Result := 'Close YIEN before installing or upgrading, then run Setup again. YIEN was not closed.';
end;

function InitializeUninstall(): Boolean;
begin
  Result := not CheckForMutexes('Local\YIENRunning');
  if not Result then
    SuppressibleMsgBox('Close YIEN before uninstalling, then run the uninstaller again. YIEN was not closed.', mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: string;
begin
  if CurUninstallStep = usUninstall then begin
    DataDir := ExpandConstant('{localappdata}\YIEN');
    if DirExists(DataDir) then begin
      if UninstallSilent then
        Exit;
      if MsgBox('Keep your YIEN data and saved sessions in ' + DataDir + '?', mbConfirmation, MB_YESNO or MB_DEFBUTTON1) = IDNO then
        DelTree(DataDir, True, True, True);
    end;
  end;
end;

