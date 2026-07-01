Sub ReadDbgAtlasDoctorSummary()
  On Error Resume Next

  Dim installRoot
  installRoot = Session.Property("INSTALLROOT")
  If Len(installRoot) > 0 And Right(installRoot, 1) <> "\" Then
    installRoot = installRoot & "\"
  End If

  Dim summaryPath
  summaryPath = installRoot & "var\log\postinstall-summary.txt"

  Dim text
  text = ""

  Dim fso
  Set fso = CreateObject("Scripting.FileSystemObject")
  If Len(summaryPath) > 0 And fso.FileExists(summaryPath) Then
    Dim stream
    Set stream = fso.OpenTextFile(summaryPath, 1, False, -2)
    text = stream.ReadAll
    stream.Close
  End If

  If Len(Trim(text)) = 0 Then
    text = "DbgAtlas installation completed, but the post-install diagnostic summary could not be displayed. See the MSI log or the install root var\log directory."
  End If

  If Len(text) > 900 Then
    text = Left(text, 897) & "..."
  End If

  Session.Property("WIXUI_EXITDIALOGOPTIONALTEXT") = text
End Sub
