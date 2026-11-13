# Codepluse 拉取后自动更新

`git pull` 只更新源码。桌面端实际读取 `codeg-ui/out` 的静态页面和已编译的 Go 程序；运行中的窗口及后台也不会自行重新加载，因此仅拉取后看起来仍是旧版本。

在每台 Windows 工作副本上执行一次：

```powershell
pnpm codepluse:setup-auto-update
```

之后每次 `git pull` 成功合并更新，Git 的 `post-merge` hook 会自动：

1. 关闭当前正在运行的 Codepluse 窗口，并记录它使用的端口和页面。
2. 构建 Next.js 静态前端、Go 后台和桌面程序。
3. 重启之前正在运行的后台服务和桌面窗口。

如果 Codepluse 当时未运行，hook 只构建；下次从快捷方式打开时会载入新版本。构建依赖 Node.js、pnpm 和 Go。首次缺少前端依赖时会自动安装。

hook 会请求 Codepluse 正常关闭；若窗口在 20 秒内未退出，更新停止，不会强制终止窗口。关闭后手动执行以下命令即可完成构建和重启：

```powershell
pnpm codepluse:refresh
```

Git 不会随仓库自动启用 hooks，所以每个工作副本都需要运行一次安装命令。Git 的 `post-merge` hook 会在 `git pull` 完成合并后触发。[Git hooks 文档](https://git-scm.com/docs/githooks#_post_merge)
