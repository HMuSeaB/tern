; NSIS 安装钩子：装之前先让占用文件的进程让路。
;
; # 为什么非有不可
;
; tern-agent.exe 是常驻进程，而 Windows 上**覆盖不了正在运行的 exe**。
; 不先结束它，每次升级都会停在
;     Can't write: D:\Tools\tern\tern-agent.exe
; 然后整个安装回滚——用户看到的是"装不上"，实际只是旧版还占着文件。
;
; 这不是偶发：agent 的设计就是关窗口不停机（用户只想看用量时不该断流量），
; 所以只要用户装过一次，此后每次升级都必然撞上。
;
; # 正常路径不该走到这儿
;
; 面板从托盘「退出」时会先 POST /api/agent/exit 让 agent 优雅收尾
; （在途请求补记 aborted、写入队列排空，用量不丢）。所以用户按正常方式
; 退出再装，这里两个进程都不在，钩子什么都不做。
;
; 这个钩子兜的是**没正常退出就直接装**的情况：任务栏还开着面板、
; 或者用户根本不知道有 agent 在后台。此时只能硬杀——用量的那一点损失
; （最多一条 in-flight 请求的 aborted 补记）换来"能装上"。
;
; 用 taskkill 而不是 curl：agent 的 token 在 tern.json 里，安装器不知道，
; 带错 token 只会吃一个 401。而 taskkill 不需要任何凭据。

!macro NSIS_HOOK_PREINSTALL
  DetailPrint "结束可能占用文件的 tern 进程…"

  ; /F 强杀、/T 连子进程、>NUL 静默。进程不存在时 taskkill 报错，
  ; 那是正常情况（用户正常退出过），不影响安装
  nsExec::ExecToLog 'taskkill /F /T /IM tern-agent.exe'
  Sleep 500

  nsExec::ExecToLog 'taskkill /F /T /IM tern-app.exe'
  Sleep 500

  ; 文件锁偶尔会滞留一个节流窗口。再确认一次，多等一轮
  ; 比让用户看完整回滚好——那个进度条都走到一半了
  FindProcDLL::FindProc "tern-agent.exe"
  Pop $R0
  ${If} $R0 == 1
    DetailPrint "常驻进程仍在，再等一轮…"
    Sleep 1500
    nsExec::ExecToLog 'taskkill /F /T /IM tern-agent.exe'
    Sleep 800
  ${EndIf}
!macro_end
