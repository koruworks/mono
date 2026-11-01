// SPDX-License-Identifier: MPL-2.0
//
// Times one desktop app launch from outside the process. Loaded by
// launch-bench.ps1 through Add-Type; see that script for how it is used.
//
// From outside, because the number a person feels starts before the
// executable's own code runs: process creation and the loader resolving every
// imported DLL come first, and Koru's in-app "first frame presented in" line
// cannot see them.
//
//   t0         just before CreateProcess (or packaged-app activation)
//   visible    the first top-level window of the new process that is visible,
//              not DWM-cloaked and larger than 200x150
//   responsive after visible, the window answers a WM_NULL round trip: its
//              thread is pumping messages, so a keystroke would be handled
//   foreground the window is the foreground window (keyboard focus)
//
// There is deliberately no "pixels stopped changing" measure: a blinking caret
// changes them every half second, which made it measure the caret.

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;

public static class LaunchBench
{
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr res);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint ms);

    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }

    [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId, [MarshalAs(UnmanagedType.LPWStr)] string arguments, int options, out uint processId);
    }
    [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")] class ApplicationActivationManager { }

    public class Result {
        public uint Pid; public IntPtr Hwnd;
        public double VisibleMs = -1, ResponsiveMs = -1, ForegroundMs = -1;
        public string Error;
    }

    static bool Usable(IntPtr h)
    {
        if (!IsWindowVisible(h)) return false;
        if (GetWindow(h, 4 /* GW_OWNER */) != IntPtr.Zero) return false;
        int cloaked;
        if (DwmGetWindowAttribute(h, 14 /* DWMWA_CLOAKED */, out cloaked, 4) == 0 && cloaked != 0) return false;
        RECT r;
        if (!GetWindowRect(h, out r)) return false;
        return (r.R - r.L) > 200 && (r.B - r.T) > 150;
    }

    // Process names for pids born after t0, looked up once each. Only new pids
    // are ever looked up: asking for the name of every windowed process on the
    // machine costs a full process snapshot each, and the first version of
    // this added two seconds to every launch it timed by doing exactly that.
    static readonly Dictionary<uint, string> names = new Dictionary<uint, string>();
    static string NameOf(uint pid)
    {
        string n;
        if (!names.TryGetValue(pid, out n)) {
            try { n = Process.GetProcessById((int)pid).ProcessName; } catch { n = ""; }
            names[pid] = n;
        }
        return n;
    }

    // A window of `pid`, or of any process named `name` that was not alive
    // before the launch: a launch stub or packaged-app broker hands the window
    // to a process other than the one started.
    static IntPtr FindWindow(uint pid, string name, HashSet<uint> before, out uint owner)
    {
        IntPtr found = IntPtr.Zero; uint who = 0;
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            bool mine = p == pid || (!string.IsNullOrEmpty(name) && !before.Contains(p)
                && string.Equals(NameOf(p), name, StringComparison.OrdinalIgnoreCase));
            if (mine && Usable(h)) { found = h; who = p; return false; }
            return true;
        }, IntPtr.Zero);
        owner = who;
        return found;
    }

    // `aumid` set: packaged-app activation. Otherwise CreateProcess on `exe`.
    // PowerShell hands $null to a string parameter as "", hence IsNullOrEmpty.
    public static Result Run(string exe, string args, string aumid, string procName, int timeoutMs)
    {
        timeBeginPeriod(1);
        var res = new Result();
        var before = new HashSet<uint>();
        foreach (var p in Process.GetProcesses()) before.Add((uint)p.Id);
        names.Clear();
        var sw = new Stopwatch();
        try {
            if (!string.IsNullOrEmpty(aumid)) {
                var mgr = (IApplicationActivationManager)new ApplicationActivationManager();
                sw.Start();
                uint pid;
                int hr = mgr.ActivateApplication(aumid, args ?? "", 0, out pid);
                if (hr != 0) { res.Error = "ActivateApplication hr=0x" + hr.ToString("x"); return res; }
                res.Pid = pid;
            } else {
                // Output captured and thrown away: a Koru build attaches to the
                // parent console and logs to it, which buried the table. Drained
                // rather than merely redirected, so a full pipe cannot stall the
                // app being timed.
                var psi = new ProcessStartInfo(exe, args ?? "") {
                    UseShellExecute = false,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                };
                sw.Start();
                var started = Process.Start(psi);
                res.Pid = (uint)started.Id;
                started.OutputDataReceived += (s, e) => { };
                started.ErrorDataReceived += (s, e) => { };
                started.BeginOutputReadLine();
                started.BeginErrorReadLine();
            }
            while (sw.ElapsedMilliseconds < timeoutMs) {
                uint owner;
                IntPtr h = FindWindow(res.Pid, procName, before, out owner);
                if (h != IntPtr.Zero) {
                    res.Hwnd = h; res.Pid = owner; res.VisibleMs = sw.Elapsed.TotalMilliseconds;
                    break;
                }
                Thread.Sleep(1);
            }
            if (res.Hwnd == IntPtr.Zero) { res.Error = "no window within " + timeoutMs + " ms"; return res; }
            IntPtr ignored;
            if (SendMessageTimeout(res.Hwnd, 0 /* WM_NULL */, IntPtr.Zero, IntPtr.Zero, 0x2 /* SMTO_ABORTIFHUNG */, (uint)timeoutMs, out ignored) != IntPtr.Zero)
                res.ResponsiveMs = sw.Elapsed.TotalMilliseconds;
            // Focus can arrive a little after the first message is answered.
            while (sw.ElapsedMilliseconds < res.VisibleMs + 2000) {
                if (GetForegroundWindow() == res.Hwnd) { res.ForegroundMs = sw.Elapsed.TotalMilliseconds; break; }
                Thread.Sleep(1);
            }
        } catch (Exception e) { res.Error = e.Message; }
        return res;
    }

    public static void Close(IntPtr hwnd) { PostMessage(hwnd, 0x0010 /* WM_CLOSE */, IntPtr.Zero, IntPtr.Zero); }
}
