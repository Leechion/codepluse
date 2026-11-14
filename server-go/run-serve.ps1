# codeg.Server.static 写死 s.Root/codeg-ui/out，因此必须从项目根启动
Set-Location "E:\code\codepluse"
$env:HARNESS_HUB_CODEG = "1"
$env:PORT = "8840"
$env:HARNESS_HUB_ALLOW_ANY_PATH = "1"
& "E:\code\codepluse\server-go\serve-codeg.exe"
