// W3Quen.asi's Tracy instrumentation (w3\README.md, Tracy), ported from the W3Trace ASI: a Tracy zone on each witcher3.exe
// function in hooks.inc, frame marks, per-frame plots and call counters. Dormant unless quen's tracy launch profile
// triggered it (Triggered): only then does trace::Init, from DllMain, hook the functions (Detours) and start Tracy on a
// thread of its own. Otherwise nothing here runs: no hook, no thread, no socket, no allocation. Tracy's client is built
// to record only while a capture is connected, on 127.0.0.1 only, without broadcast (asi\build.bat). W3Quen.cpp
// includes this file after defining Log(format, args...), the one translation unit of the ASI.
// Left out of W3Trace's: the GPU debug markers (BeginEvent / EndEvent on the engine's command lists, and its three
// hooks GpuScope_Begin, GpuScope_End, GpuCtx_Reset), the round-3 hash probes (W3Trace's README: they cost time on the
// critical path), its ini, and the host-name gate (the PE stamp and each function's entry bytes are the gates).
#pragma once
#include <windows.h>
#include <intrin.h>
#include <tlhelp32.h>
#include <atomic>
#include <climits>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <new>
#include <vector>
#include <detours.h>
// A zone's source file, as Tracy records it: a name, not __FILE__ (the build folder's path would be in the ASI).
#define TracyFile "W3Quen.asi"
#include <tracy/Tracy.hpp>
#include <tracy/TracyC.h>

namespace trace {

// The trigger: quen's tracy launch profile puts W3_QUEN_TRACY=1 in the game's environment, beside W3_QUEN_TOKEN and
// W3_QUEN_PID. quen passes the game no other variable of its own environment and none from the agent.
inline bool Triggered() {
  static const bool on = [] {
    wchar_t v[4]{};
    return GetEnvironmentVariableW(L"W3_QUEN_TRACY", v, 4) == 1 && v[0] == L'1' &&
           GetEnvironmentVariableW(L"W3_QUEN_TOKEN", nullptr, 0) != 0;
  }();
  return on;
}

// In a tracy launch every allocation of this module goes to a private heap, Detours's bookkeeping included: Install
// suspends the other threads, and one of them may hold the process heap's lock at that moment (W3Trace). Otherwise
// malloc, as the default operator new does, so a dormant W3Quen allocates as 0.1.2 did.
inline HANDLE Heap() {
  static const HANDLE heap = Triggered() ? HeapCreate(0, 0, 0) : nullptr;
  return heap;
}

}  // namespace trace

void* operator new(size_t size) {
  if (!size) size = 1;
  const HANDLE heap = trace::Heap();
  if (void* p = heap ? HeapAlloc(heap, 0, size) : malloc(size)) return p;
  throw std::bad_alloc();
}
void operator delete(void* p) noexcept {
  if (!p) return;
  if (const HANDLE heap = trace::Heap()) HeapFree(heap, 0, p);
  else free(p);
}
void operator delete(void* p, size_t) noexcept { operator delete(p); }

namespace trace {

using U = uint64_t;
using D = double;

// lfence before each zone end, so the end timestamp (rdtsc, not serializing) is not taken before the call's loads have
// completed (W3Trace, from W3BoneFix: unfenced zones closed early on uncached reads).
#define W3_ZONE_FENCE() _mm_lfence()

// The build every RVA here and in hooks.inc is for: witcher3.exe Steam build 26238013's PE stamp (5.01's: 0x6AC555A3).
// FUN_ and DAT_ addresses in the [O] notes below are 5.01's, where they were observed.
constexpr uint32_t kTimeDateStamp = 0x6ADFBD7A;
constexpr int kRingSlowUs = 20;  // RenderRing_Alloc calls at least this slow get a zone of their own (W3Trace's default)
// [O] (W3Trace, re/cpu_pt_main.md) the global holding the GpuApi pointer whose +0x31b71ac is the engine's fence-wait time
// (5.01: 0x5a58ef8)
constexpr uintptr_t kEngineFenceRva = 0x5a56ef8;
// [O] (W3Trace) return addresses of the three calls to JobCounter_Wait in Render_FrameTaskGraph's end-of-frame join
// (0x1d321a0: calls at 0x1d330a4 / 0x1d330c2 / 0x1d330f4, E8 to 0x1d53270; 5.01: 0x1d335a9, 0x1d335c7, 0x1d335f9).
constexpr uintptr_t kJoinRets[3] = {0x1d330a9, 0x1d330c7, 0x1d330f9};

static uintptr_t g_base, g_image_size;  // the image the RVAs and job-name pointers refer to
static std::atomic<bool> g_live;        // Tracy started: wrappers record; before that they only forward
static double g_ticks_per_us = 1;       // Tracy timer (TSC) ticks per microsecond, measured by StartTracing
static int64_t g_ring_slow_ticks = INT64_MAX;
static std::atomic<int64_t> g_ring_ticks, g_ring_calls;  // RenderRing_Alloc totals since the last game tick
static std::atomic<int> g_dispatchers;

// All 8 register/stack argument slots are forwarded whatever the real count (<= 8): extra slots carry whatever the
// caller left in r8/r9 or its frame, which the callee ignores. A float slot (XMM) is declared double so all 64 bits pass
// unchanged. Every wrapper returns RAX (void, int and pointer returns; no hook returns in XMM0). [O] (W3Trace's /FAs
// listing) the wrappers never write the caller's stack-argument slots, so forwarding 8 slots to a 1-argument function
// is safe.
#define W3_PARAMS_I (U a0, U a1, U a2, U a3, U a4, U a5, U a6, U a7)
#define W3_PARAMS_F1 (U a0, D a1, U a2, U a3, U a4, U a5, U a6, U a7)
#define W3_ARGS (a0, a1, a2, a3, a4, a5, a6, a7)
#define W3_DECLARE(NAME, SIG) \
  static U(*NAME##_orig) W3_PARAMS_##SIG; \
  static U NAME##_hook W3_PARAMS_##SIG
#define W3_HOOK_plain(NAME, SIG) \
  W3_DECLARE(NAME, SIG) { \
    if (!g_live.load(std::memory_order_relaxed)) return NAME##_orig W3_ARGS; \
    ZoneScopedN(#NAME); \
    const U r = NAME##_orig W3_ARGS; \
    W3_ZONE_FENCE(); \
    return r; \
  }
#define W3_HOOK_custom(NAME, SIG) W3_DECLARE(NAME, SIG);  // defined below
// probe: a plain zone that also calls NAME##_probe (written by hand below) at entry (post false) and after the call
// (post true, r = the return value), inside the zone: a = the 8 argument slots, ret = the caller's return address.
#define W3_HOOK_probe(NAME, SIG) \
  static void NAME##_probe(bool post, const U* a, U r, uintptr_t ret, tracy::ScopedZone* zone); \
  W3_DECLARE(NAME, SIG) { \
    if (!g_live.load(std::memory_order_relaxed)) return NAME##_orig W3_ARGS; \
    ZoneScopedN(#NAME); \
    const U a[8] = {a0, a1, a2, a3, a4, a5, a6, a7}; \
    const uintptr_t ret = reinterpret_cast<uintptr_t>(_ReturnAddress()); \
    NAME##_probe(false, a, 0, ret, &___tracy_scoped_zone); \
    const U r = NAME##_orig W3_ARGS; \
    W3_ZONE_FENCE(); \
    NAME##_probe(true, a, r, ret, &___tracy_scoped_zone); \
    return r; \
  }
// count: no C++ wrapper; MakeCountThunks gives the hook a thunk that counts calls (CountSlot below).
#define W3_HOOK_count(NAME, SIG) static void* NAME##_orig;
#define W3_HOOK(NAME, RVA, BYTES, SIG, KIND) W3_HOOK_##KIND(NAME, SIG)
#include "hooks.inc"
#undef W3_HOOK
// W3BoneFix.asi swaps GetCustomBoneMatrix 0x1CABAB0..0x1CABB6F through its vtable slot (hooks.tsv 0x37AC6F0).
#define W3_HOOK(NAME, RVA, BYTES, SIG, KIND) static_assert(RVA + 16 <= 0x1CABAB0 || RVA >= 0x1CABB70, #NAME);
#include "hooks.inc"
#undef W3_HOOK

// A count hook's counter and jump slot, on the RW page next to its thunk (MakeCountThunks).
struct CountSlot {
  std::atomic<uint64_t> calls;
  void* orig;  // the thunk jumps through this: the target before install, Detours's trampoline after
};
struct Hook {
  const char* name;
  uint32_t rva;
  const char* bytes;  // 16 expected entry bytes
  void** orig;        // the wrapper's pointer to the original: the target before install, the trampoline after
  void* hook;         // the wrapper (count hooks: the thunk, set by MakeCountThunks)
  void* target;       // set by Init to install this hook
  const char* status; // nullptr until decided; "ok" once installed
  LONG error;
  bool count;         // wrapper kind "count"
  CountSlot* slot;    // count hooks: the counter (MakeCountThunks)
  bool chained;       // installed over another hook's jmp (EntryGate)
};
#define W3_FN_plain(NAME) reinterpret_cast<void*>(&NAME##_hook)
#define W3_FN_custom(NAME) W3_FN_plain(NAME)
#define W3_FN_probe(NAME) W3_FN_plain(NAME)
#define W3_FN_count(NAME) nullptr
#define W3_IS_COUNT_plain false
#define W3_IS_COUNT_custom false
#define W3_IS_COUNT_probe false
#define W3_IS_COUNT_count true
static Hook g_hooks[] = {
#define W3_HOOK(NAME, RVA, BYTES, SIG, KIND) \
  {#NAME, RVA, BYTES, reinterpret_cast<void**>(&NAME##_orig), W3_FN_##KIND(NAME), nullptr, nullptr, 0, \
   W3_IS_COUNT_##KIND, nullptr, false},
#include "hooks.inc"
#undef W3_HOOK
};

// The entry-byte gate. "ok": the live bytes are hooks.inc's. "chained": another hook replaced the entry with a jmp rel32
// (Detours, MinHook): Detours copies that jmp into our trampoline with its target fixed up (detours.cpp: E9 is
// ENTRY_CopyBytes3Or5Target and ends the copy), so both hooks run, ours first. nullptr: anything else.
static const char* EntryGate(const uint8_t* live, const char* expect) {
  if (!memcmp(live, expect, 16)) return "ok";
  return live[0] == 0xE9 ? "chained" : nullptr;
}

// Count hooks: one 32-byte thunk each, `lock inc qword [rip+slot.calls]; jmp qword [rip+slot.orig]`. It changes only the
// flags (volatile across calls), so every argument (integer, XMM, stack) and the return value pass untouched, whatever
// the target's signature. Code on its own pages (RX after writing), slots on the RW pages after them (RIP-relative,
// < 2 GB apart): no stores near executing code. false: no memory (the count hooks then get no hook and stay out).
static bool MakeCountThunks(Hook* hooks, size_t count) {
  size_t n = 0;
  for (size_t i = 0; i < count; ++i) n += hooks[i].count;
  if (!n) return true;
  const size_t code_size = (n * 32 + 4095) & ~size_t{4095}, data_size = (n * sizeof(CountSlot) + 4095) & ~size_t{4095};
  auto* code = static_cast<uint8_t*>(VirtualAlloc(nullptr, code_size + data_size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE));
  if (!code) return false;
  auto* slots = reinterpret_cast<CountSlot*>(code + code_size);
  static constexpr uint8_t kThunk[16] = {0xF0, 0x48, 0xFF, 0x05, 0, 0, 0, 0, 0xFF, 0x25, 0, 0, 0, 0, 0xCC, 0xCC};
  for (size_t i = 0, k = 0; i < count; ++i) {
    if (!hooks[i].count) continue;
    uint8_t* t = code + k * 32;
    CountSlot* s = new (&slots[k++]) CountSlot{};
    memcpy(t, kThunk, sizeof(kThunk));
    const auto calls = static_cast<int32_t>(reinterpret_cast<uint8_t*>(&s->calls) - (t + 8));  // rip = end of the inc
    const auto orig = static_cast<int32_t>(reinterpret_cast<uint8_t*>(&s->orig) - (t + 14));   // rip = end of the jmp
    memcpy(t + 4, &calls, 4);
    memcpy(t + 10, &orig, 4);
    hooks[i].slot = s;
    hooks[i].hook = t;
    hooks[i].orig = &s->orig;
  }
  DWORD old;
  VirtualProtect(code, code_size, PAGE_EXECUTE_READ, &old);
  FlushInstructionCache(GetCurrentProcess(), code, code_size);
  return true;
}

static void NameThread(const char* role) {
  thread_local bool named;
  if (!named) {
    named = true;
    tracy::SetThreadName(role);  // also the thread's Windows description
  }
}

// The engine's own timing of the main thread's fence wait, read where CGame_Tick stores it (float at
// [image+kEngineFenceRva]+0x31b71ac, per W3Trace's agent C; units not established). false if unreadable.
static bool EngineFenceWait(float* out) {
  __try {
    const uintptr_t p = *reinterpret_cast<const uintptr_t*>(g_base + kEngineFenceRva);
    if (!p) return false;
    *out = *reinterpret_cast<const float*>(p + 0x31b71ac);
    return true;
  } __except (EXCEPTION_EXECUTE_HANDLER) {
    return false;
  }
}

// Main thread, once per game frame: the "game" frame mark, and the per-frame plots (RenderRing_Alloc totals from all
// threads since the previous tick; the engine's own fence-wait value from the previous frame).
static U CBaseEngine_Tick_hook W3_PARAMS_F1 {
  if (!g_live.load(std::memory_order_relaxed)) return CBaseEngine_Tick_orig W3_ARGS;
  NameThread("main");
  FrameMarkNamed("game");
  TracyPlot("RenderRing_Alloc calls", g_ring_calls.exchange(0, std::memory_order_relaxed));
  TracyPlot("RenderRing_Alloc us", g_ring_ticks.exchange(0, std::memory_order_relaxed) / g_ticks_per_us);
  float wait;
  if (EngineFenceWait(&wait)) TracyPlot("engine fence wait (raw)", wait);
  ZoneScopedN("CBaseEngine_Tick");
  const U r = CBaseEngine_Tick_orig W3_ARGS;
  W3_ZONE_FENCE();
  return r;
}

static void PlotFrameCounters();  // below

// Render thread: the "render" frame mark right after EndFrame's Execute (W3Trace: NewFrame's body has no .pdata entry),
// then the per-frame counter plots.
static U RenderCmd_EndFrame_Exec_hook W3_PARAMS_I {
  if (!g_live.load(std::memory_order_relaxed)) return RenderCmd_EndFrame_Exec_orig W3_ARGS;
  NameThread("render");
  U r;
  {
    ZoneScopedN("RenderCmd_EndFrame_Exec");
    r = RenderCmd_EndFrame_Exec_orig W3_ARGS;
    W3_ZONE_FENCE();
  }
  FrameMarkNamed("render");
  PlotFrameCounters();
  return r;
}

// "<job name> 0x<invoker RVA>" without commas or newlines (tracy-csvexport writes zone text unquoted).
static int JobText(char* buf, size_t size, uintptr_t name, uintptr_t invoker) {
  const auto in_image = [](uintptr_t p) { return p - g_base < g_image_size; };
  size_t n = 0;
  if (in_image(name))
    for (; n < 48 && in_image(name + n) && reinterpret_cast<const char*>(name)[n]; ++n) {
      const char c = reinterpret_cast<const char*>(name)[n];
      buf[n] = c > ' ' && c < 127 && c != ',' ? c : '_';
    }
  if (!n) n = static_cast<size_t>(sprintf_s(buf, size, "?"));
  return static_cast<int>(n) + sprintf_s(buf + n, size - n, in_image(invoker) ? " 0x%llx" : " abs 0x%llx",
                                         static_cast<unsigned long long>(in_image(invoker) ? invoker - g_base : invoker));
}

// RenderDispatcher worker: the pop (idle spin + semaphore wait) gets its own zone; the job that follows gets a "Job"
// zone, text = job name + invoker RVA, from the pop's return to this thread's next pop. [O] (W3Trace) FUN_141d53850
// returns only with a job copied to out (a1): +0x00 name (const char*), +0x10 invoker. [O] The worker loop FUN_141d53ea0
// also runs jobs from a per-worker local queue without calling the pop; such jobs stay inside the previous Job zone.
static thread_local TracyCZoneCtx t_job;
static U RenderDispatcher_JobPop_hook W3_PARAMS_I {
  if (!g_live.load(std::memory_order_relaxed)) return RenderDispatcher_JobPop_orig W3_ARGS;
  TracyCZoneEnd(t_job);
  t_job = {};
  thread_local bool named;
  if (!named) {
    named = true;
    char name[32];
    sprintf_s(name, "dispatcher_%d", ++g_dispatchers);
    tracy::SetThreadName(name);
  }
  U r;
  {
    ZoneScopedN("RenderDispatcher_JobPop");
    r = RenderDispatcher_JobPop_orig W3_ARGS;
    W3_ZONE_FENCE();
  }
  const auto* job = reinterpret_cast<const uintptr_t*>(a1);
  TracyCZoneN(ctx, "Job", 1);
  if (ctx.active) {
    char text[80];
    TracyCZoneText(ctx, text, JobText(text, sizeof(text), job[0], job[2]));
  }
  t_job = ctx;
  return r;
}

// Job-counter spin: the end-of-frame join gets its own zone name; every wait carries its return RVA as text.
static U JobCounter_Wait_hook W3_PARAMS_I {
  if (!g_live.load(std::memory_order_relaxed)) return JobCounter_Wait_orig W3_ARGS;
  const uintptr_t ret = reinterpret_cast<uintptr_t>(_ReturnAddress()) - g_base;
  const bool join = ret == kJoinRets[0] || ret == kJoinRets[1] || ret == kJoinRets[2];
  static constexpr tracy::SourceLocationData kJoin{"JobCounter_Wait_FrameJoin", __FUNCTION__, TracyFile, __LINE__, 0};
  static constexpr tracy::SourceLocationData kOther{"JobCounter_Wait", __FUNCTION__, TracyFile, __LINE__, 0};
  tracy::ScopedZone zone(join ? &kJoin : &kOther);
  char text[32];
  zone.Text(text, static_cast<size_t>(sprintf_s(text, "ret 0x%llx", static_cast<unsigned long long>(ret))));
  const U r = JobCounter_Wait_orig W3_ARGS;
  W3_ZONE_FENCE();
  return r;
}

// A zone with timestamps taken earlier on this thread. Same queue items as tracy::ScopedZone; valid as long as this
// thread queued nothing else between t0 and t1.
static void EmitZone(const tracy::SourceLocationData* loc, int64_t t0, int64_t t1) {
  {
    TracyQueuePrepare(tracy::QueueType::ZoneBegin);
    tracy::MemWrite(&item->zoneBegin.time, t0);
    tracy::MemWrite(&item->zoneBegin.srcloc, reinterpret_cast<uint64_t>(loc));
    TracyQueueCommit(zoneBeginThread);
  }
  {
    TracyQueuePrepare(tracy::QueueType::ZoneEnd);
    tracy::MemWrite(&item->zoneEnd.time, t1);
    TracyQueueCommit(zoneEndThread);
  }
}

// Command-ring allocation, up to thousands of calls per frame [U]: time accumulated (plotted per game tick), and only
// calls slower than kRingSlowUs (ring full: Sleep loops) get a zone, RenderRing_Alloc_slow.
static U RenderRing_Alloc_hook W3_PARAMS_I {
  if (!g_live.load(std::memory_order_relaxed)) return RenderRing_Alloc_orig W3_ARGS;
  const int64_t t0 = tracy::Profiler::GetTime();
  const U r = RenderRing_Alloc_orig W3_ARGS;
  W3_ZONE_FENCE();
  const int64_t t1 = tracy::Profiler::GetTime();
  g_ring_ticks.fetch_add(t1 - t0, std::memory_order_relaxed);
  g_ring_calls.fetch_add(1, std::memory_order_relaxed);
  static constexpr tracy::SourceLocationData kSlow{"RenderRing_Alloc_slow", __FUNCTION__, TracyFile, __LINE__, 0};
  if (t1 - t0 >= g_ring_slow_ticks && TracyIsConnected) EmitZone(&kSlow, t0, t1);
  return r;
}

// ==== W3Trace's round 3 (2026-10-14; re/cpu_pt_hotspots.md): zone texts and per-frame counters ====

// Engine memory under SEH: false if unreadable.
template <class T>
__declspec(noinline) static bool ReadAt(uintptr_t p, T* out) {
  __try {
    *out = *reinterpret_cast<const T*>(p);
    return true;
  } __except (EXCEPTION_EXECUTE_HANDLER) {
    return false;
  }
}

template <class... A>
static void SetZoneText(tracy::ScopedZone* zone, const char* fmt, A... args) {  // no commas or newlines: csvexport
  char text[96];
  const int n = sprintf_s(text, fmt, args...);
  if (n > 0) zone->Text(text, static_cast<size_t>(n));
}

enum { kWalkRecords, kQueueEntries, kSbtBytes, kFixupBarriers, kCloseBarriers, kCounters };
static const char* const kCounterNames[kCounters] = {"RT walk records", "RT queue entries", "SBT bytes",
                                                     "Fixup barriers", "Close barriers"};
static std::atomic<int64_t> g_counters[kCounters];
static void Count(int k, int64_t v) { g_counters[k].fetch_add(v, std::memory_order_relaxed); }

// Render thread, once per frame (RenderCmd_EndFrame_Exec): every installed count hook's calls and the counters above.
static void PlotFrameCounters() {
  for (const Hook& h : g_hooks)
    if (h.slot && h.status && !strcmp(h.status, "ok"))
      TracyPlot(h.name, static_cast<int64_t>(h.slot->calls.exchange(0, std::memory_order_relaxed)));
  for (int k = 0; k < kCounters; ++k) TracyPlot(kCounterNames[k], g_counters[k].exchange(0, std::memory_order_relaxed));
}

// [O] FUN_141c1fdd0(renderer, frame ctx, hair): zone text "hair 0/1 count N", N its return value.
static void PT_LightList_probe(bool post, const U* a, U r, uintptr_t, tracy::ScopedZone* zone) {
  if (post) SetZoneText(zone, "hair %d count %u", (a[2] & 0xff) != 0 ? 1 : 0, static_cast<uint32_t>(r));
}

// [O] FUN_140352130(lights, n, &camera): the sort of n lights.
static void PT_LightSort_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  if (post) return;
  const auto n = static_cast<uint32_t>(a[1]);
  SetZoneText(zone, "n %u", n);
  TracyPlot("PT light sort n", static_cast<int64_t>(n));
}

// [O] FUN_141c19d60 recurses into itself: only the outermost call gets a zone; its text counts the calls inside it.
static thread_local uint32_t t_sort_depth, t_sort_calls;
static U PT_LightSortLoop_hook W3_PARAMS_I {
  if (t_sort_depth || !g_live.load(std::memory_order_relaxed)) {
    ++t_sort_calls;
    ++t_sort_depth;
    const U r = PT_LightSortLoop_orig W3_ARGS;
    --t_sort_depth;
    return r;
  }
  ZoneScopedN("PT_LightSortLoop");
  t_sort_calls = 1;
  t_sort_depth = 1;
  const U r = PT_LightSortLoop_orig W3_ARGS;
  W3_ZONE_FENCE();
  t_sort_depth = 0;
  SetZoneText(&___tracy_scoped_zone, "calls %u", t_sort_calls);
  return r;
}

// [O] FUN_141c723b0(renderer): a light-list upload; zone text = its call site.
static void LightBlock_Upload_probe(bool post, const U*, U, uintptr_t ret, tracy::ScopedZone* zone) {
  if (!post) SetZoneText(zone, "ret 0x%llx", static_cast<unsigned long long>(ret - g_base));
}

// An image string (FUN_141cfa940's phase names) into buf, else "?".
static void ImageString(uintptr_t p, char* buf, size_t size) {
  size_t n = 0;
  for (char c; n + 1 < size && p + n - g_base < g_image_size && ReadAt(p + n, &c) && c > ' ' && c < 127 && c != ','; ++n)
    buf[n] = c;
  if (!n) buf[n++] = '?';
  buf[n] = 0;
}

// [O] FUN_140388810(batch, phase name): one phase batch submitted; batch+0xe0 = its first phase index.
static void Submit_Phase_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  if (post) return;
  uint32_t phase = UINT32_MAX;
  ReadAt(a[0] + 0xe0, &phase);
  char name[32];
  ImageString(a[1], name, sizeof(name));
  SetZoneText(zone, "%s %u", name, phase);
}

// Pending barriers ([O] ctx+0x210484) of FUN_140508a60's slot and command-list range: its fixups append to them.
static int64_t PendingBarriers(uintptr_t slot, uintptr_t range) {
  int64_t total = 0;
  uintptr_t ctx, p, end;
  uint32_t n;
  if (ReadAt(slot, &ctx) && ctx && ReadAt(ctx + 0x210484, &n)) total += n;
  if (!ReadAt(range, &p) || !ReadAt(range + 8, &end)) return total;
  for (int i = 0; p < end && i < 64; p += 8, ++i)
    if (ReadAt(p, &ctx) && ctx && ReadAt(ctx + 0x210484, &n)) total += n;
  return total;
}
static thread_local int64_t t_fixup_before;
static void Submit_Fixups_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  const int64_t now = PendingBarriers(a[0], a[1]);
  if (!post) {
    t_fixup_before = now;
    return;
  }
  Count(kFixupBarriers, now - t_fixup_before);
  SetZoneText(zone, "barriers %lld", now - t_fixup_before);
}

// [O] FUN_141ef4680(ctx): flushes ctx+0x210484 pending barriers, then Close.
static void GpuCtx_Close_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  uint32_t n;
  if (post || !ReadAt(a[0] + 0x210484, &n)) return;
  Count(kCloseBarriers, n);
  SetZoneText(zone, "barriers %u", n);
}

// [O] FUN_141f22b70(table) = RTShaderTable::Create: rebuilds when table+0x58 != frame DAT_145c92ae8 (here 0x5c90ae8).
static void RTSBT_Build_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  int32_t built, now, chunks = 0, extra = -1, chunks2 = 0;
  uint8_t per = 0;
  if (post || !ReadAt(a[0] + 0x58, &built) || !ReadAt(g_base + 0x5c90ae8, &now) || built == now) return;
  ReadAt(a[0] + 0x32, &per);
  ReadAt(a[0] + 0x198, &chunks);
  ReadAt(a[0] + 0x19c, &extra);
  ReadAt(a[0] + 0x140, &chunks2);
  const int64_t records = int64_t{per} * chunks + (extra != -1 ? extra : int64_t{per} * chunks2);
  Count(kSbtBytes, records * 0xa0);
  SetZoneText(zone, "records %lld bytes %lld", records, records * 0xa0);
}

// [O] the walk loops i = 1 .. *(u32*)(map+0x5028f0).
static void RTCull_ProximityWalk_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  uint32_t n;
  if (post || !ReadAt(a[0] + 0x5028f0, &n)) return;
  Count(kWalkRecords, n);
  SetZoneText(zone, "records %u", n);
}

// [O] FUN_141cd6ca0(desc): writes (kinds != 3, or all with DAT_14514cd40, here 0x514ad90) x desc+0x14 x desc+0x20
// entries per ring.
static void RTQueue_PushReady_probe(bool post, const U* a, U, uintptr_t, tracy::ScopedZone* zone) {
  uintptr_t kinds;
  uint32_t nk, nb, nc, k = 0;
  uint8_t all = 0;
  int32_t kind;
  if (post || !ReadAt(a[0], &kinds) || !ReadAt(a[0] + 8, &nk) || !ReadAt(a[0] + 0x14, &nb) || !ReadAt(a[0] + 0x20, &nc))
    return;
  ReadAt(g_base + 0x514ad90, &all);
  for (uint32_t i = 0; i < nk && i < 64; ++i) k += ReadAt(kinds + 4 * i, &kind) && (kind != 3 || all);
  const int64_t entries = int64_t{k} * nb * nc;
  Count(kQueueEntries, entries);
  SetZoneText(zone, "entries %lld", entries);
}

// Installs every hook that has a target and no status, in one Detours transaction with every other thread of the
// process suspended (Detours moves a thread whose IP is in bytes it overwrites). A hook Detours refuses is marked and
// the rest retried; a failed commit (a thread exited mid-way) is retried twice.
static void Install(Hook* hooks, size_t count) {
  for (int attempt = 0;;) {
    DetourTransactionBegin();
    Hook* refused = nullptr;
    for (size_t i = 0; i < count && !refused; ++i) {
      Hook& h = hooks[i];
      if (!h.target || h.status) continue;
      *h.orig = h.target;
      if ((h.error = DetourAttach(h.orig, h.hook)) != NO_ERROR) refused = &h;
    }
    if (refused) {
      DetourTransactionAbort();
      *refused->orig = refused->target;
      refused->status = "skipped: Detours refused the entry";
      continue;
    }
    std::vector<DWORD> ids;
    if (const HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0); snap != INVALID_HANDLE_VALUE) {
      THREADENTRY32 te{sizeof(te)};
      for (BOOL more = Thread32First(snap, &te); more; more = Thread32Next(snap, &te))
        if (te.th32OwnerProcessID == GetCurrentProcessId() && te.th32ThreadID != GetCurrentThreadId()) ids.push_back(te.th32ThreadID);
      CloseHandle(snap);
    }
    std::vector<HANDLE> threads;
    threads.reserve(ids.size());
    for (const DWORD id : ids)
      if (const HANDLE t = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_SET_CONTEXT, FALSE, id)) {
        threads.push_back(t);
        DetourUpdateThread(t);
      }
    const LONG error = DetourTransactionCommit();
    for (const HANDLE t : threads) CloseHandle(t);
    if (error != NO_ERROR && ++attempt < 3) {
      for (size_t i = 0; i < count; ++i)
        if (hooks[i].target && !hooks[i].status) *hooks[i].orig = hooks[i].target;
      continue;
    }
    for (size_t i = 0; i < count; ++i)
      if (hooks[i].target && !hooks[i].status) {
        hooks[i].error = error;
        hooks[i].status = error == NO_ERROR ? "ok" : "skipped: Detours commit failed";
        if (error != NO_ERROR) *hooks[i].orig = hooks[i].target;
      }
    return;
  }
}

// Starts Tracy (never shut down: game exit must not wait on it), measures its timer against QPC, then lets the wrappers
// record. Then, for quen's boot, says once that a capture connected: on demand, Tracy records only while one is.
// ponytail: it polls until one does; a tracy launch always starts its capture tool before the game.
static DWORD WINAPI Start(void*) {
  tracy::StartupProfiler();
  LARGE_INTEGER f, q0, q1;
  QueryPerformanceFrequency(&f);
  QueryPerformanceCounter(&q0);
  const int64_t t0 = tracy::Profiler::GetTime();
  Sleep(50);
  QueryPerformanceCounter(&q1);
  const int64_t t1 = tracy::Profiler::GetTime();
  g_ticks_per_us = static_cast<double>(t1 - t0) * static_cast<double>(f.QuadPart) / (1e6 * static_cast<double>(q1.QuadPart - q0.QuadPart));
  g_ring_slow_ticks = static_cast<int64_t>(kRingSlowUs * g_ticks_per_us);
  g_live.store(true);
  Log("tracy: started (timer %.1f ticks/us)", g_ticks_per_us);
  while (!TracyIsConnected) Sleep(50);
  Log("tracy: a capture connected");
  return 0;
}

// From DllMain, only when Triggered(), before the engine runs any hooked function [I: the loader loads ASIs at process
// start]: the exe must be build 26238013 (its PE stamp), and each function's entry bytes hooks.inc's (or another hook's
// jmp, chained over). The matching ones are hooked; then Tracy starts on its own thread (it starts threads and sleeps).
static void Init() {
  g_base = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
  const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS64*>(g_base + reinterpret_cast<const IMAGE_DOS_HEADER*>(g_base)->e_lfanew);
  g_image_size = nt->OptionalHeader.SizeOfImage;
  if (nt->FileHeader.TimeDateStamp != kTimeDateStamp) {
    Log("tracy: REFUSED: the exe's TimeDateStamp is 0x%08X, not witcher3.exe build 26238013's 0x%08X: nothing hooked, "
        "no Tracy", nt->FileHeader.TimeDateStamp, kTimeDateStamp);
    return;
  }
  const bool thunks = MakeCountThunks(g_hooks, _countof(g_hooks));
  for (Hook& h : g_hooks) {
    const auto* live = reinterpret_cast<const uint8_t*>(g_base + h.rva);
    if (h.rva + 16 > g_image_size) h.status = "skipped: RVA outside the image";
    else if (h.count && !thunks) h.status = "skipped: no memory for the count thunks";
    else if (const char* gate = EntryGate(live, h.bytes)) {
      h.target = const_cast<uint8_t*>(live);
      h.chained = gate[0] == 'c';
    } else {
      h.status = "skipped: entry bytes differ and are not a jmp (another mod patched it?)";
      Log("tracy: %s live bytes: %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x", h.name,
          live[0], live[1], live[2], live[3], live[4], live[5], live[6], live[7], live[8], live[9], live[10], live[11],
          live[12], live[13], live[14], live[15]);
    }
  }
  Install(g_hooks, _countof(g_hooks));
  int ok = 0;
  for (const Hook& h : g_hooks) {
    ok += h.status && !strcmp(h.status, "ok");
    Log("tracy: %-42s 0x%07X  %s%s (Detours %ld)", h.name, h.rva, h.status ? h.status : "?",
        h.chained ? ", chained over another hook's jmp" : "", h.error);
  }
  Log("tracy: %d of %zu hooks installed", ok, _countof(g_hooks));
  if (ok)
    if (const HANDLE thread = CreateThread(nullptr, 0, Start, nullptr, 0, nullptr)) CloseHandle(thread);
}

}  // namespace trace
