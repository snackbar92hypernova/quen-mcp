// Offline test of W3Quen.asi (no game): out\test\test.exe <mode> loads out\test\W3Quen.asi next to the stand-in
// WitcherNativeBridge.asi (wnb_stub.cpp) and plays both ends: quen's pipe client and the script calling the natives.
//   channel   token + this pid: every verb to the script line and back, the refusals of protocol.rs's tests, ttl, queue
//             cap, a missing reply, a stale handle, read-back data, an oversize reply, reconnects, server pid, the ACL;
//             QUEN_State lines in W3QuenState.log and the poll count before the first; camera paths: cam.path's checks
//             and QUEN_CamAt against quen-core sweep.rs's reference vectors; the scene verbs (0.1.13) and their
//             read-backs; an exec refused before any game state
//   exec      the exec verb (0.1.13), the console executor's entry bytes and a stand-in for it planted at its RVA: in
//             test.exe (not 5.01's PE stamp) refused; in tracyhost.exe called, with the line built from the verb, its
//             log, not called, refused by the state, refused when the entry bytes differ
//   notoken   no W3_QUEN_TOKEN: QUEN_Next is "", no pipe, neither log written
//   badtoken  a token with path characters: no pipe
//   wrongpid  W3_QUEN_PID names another process: the client is dropped
//   squat     the pipe name is taken before the ASI loads: it refuses to serve it
// Tracy (trace.h), each hook's entry bytes planted at its RVA first (Plant):
//   tracy-stamp    full trigger (W3_QUEN_TRACY=1, token, pid) in this exe, whose PE stamp isn't 5.01's: refused, dormant
//   mkhost         writes tracyhost.exe: this exe with witcher3.exe 5.01's PE stamp, for the modes below
//   tracy-none     tracyhost.exe, no variable at all: dormant
//   tracy-token    a token and pid, no trigger (a plain quen launch): dormant
//   tracy-notoken  W3_QUEN_TRACY=1 without a token: dormant
//   tracy-on       full trigger: every hook installed, Tracy listening on 127.0.0.1:TRACY_PORT only, a hooked call
//                  through the wrapper; the positive control of each dormancy check
//   tracy-capture  full trigger, and TRACY_CAPTURE (Tracy 0.14.1's tracy-capture.exe) records zones of a hooked call
// Dormant: every hook site's bytes as planted, no thread whose description starts with "Tracy", no Winsock loaded, no
// TCP or UDP endpoint of this process, no new heap, and no "tracy:" line in W3Quen.log.
#include <winsock2.h>  // types only: Winsock isn't linked (the dormant modes check it never loads)
#include <ws2ipdef.h>
#include <windows.h>
#include <aclapi.h>
#include <iphlpapi.h>
#include <tlhelp32.h>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <cmath>
#include <iterator>
#include <span>
#include <sstream>
#include <string>
#include <vector>
#pragma comment(lib, "advapi32")

static int g_fail;
#define CHECK(c) ((c) ? (void)0 : (void)(printf("FAIL line %d: %s\n", __LINE__, #c), ++g_fail))

static int (*g_next)(wchar_t*, int);
static int (*g_reply)(int, bool, const char*, const char*);
static int (*g_state)(int);
static int (*g_camat)(int, wchar_t*, int);
static uint64_t g_polls;  // g_next calls
static ULONGLONG g_first;  // the tick just before the first
static std::wstring g_pipe;

static std::wstring Here(const wchar_t* file) {
  wchar_t path[MAX_PATH];
  GetModuleFileNameW(nullptr, path, MAX_PATH);
  return std::wstring(path, wcsrchr(path, L'\\') + 1) + file;
}

static std::string ReadLog(const wchar_t* file = L"W3Quen.log") {
  std::ifstream in(Here(file));
  std::stringstream text;
  text << in.rdbuf();
  return text.str();
}

static void Load() {
  CHECK(LoadLibraryW(Here(L"W3Quen.asi").c_str()) != nullptr);
  const HMODULE wnb = GetModuleHandleW(L"WitcherNativeBridge.asi");
  using Registered = bool (*)(const char*);
  const auto registered = reinterpret_cast<Registered>(GetProcAddress(wnb, "Stub_Registered"));
  g_next = reinterpret_cast<decltype(g_next)>(GetProcAddress(wnb, "Stub_Next"));
  g_reply = reinterpret_cast<decltype(g_reply)>(GetProcAddress(wnb, "Stub_Reply"));
  g_state = reinterpret_cast<decltype(g_state)>(GetProcAddress(wnb, "Stub_State"));
  g_camat = reinterpret_cast<decltype(g_camat)>(GetProcAddress(wnb, "Stub_CamAt"));
  if (!registered || !g_next || !g_reply || !g_state || !g_camat) {
    printf("FAIL: the stand-in WitcherNativeBridge.asi is not loaded\n");
    exit(1);
  }
  CHECK(registered("QUEN_Next") && registered("QUEN_Reply") && registered("QUEN_State") && registered("QUEN_CamAt"));
}

static std::wstring Next() {
  wchar_t out[8192];
  if (!g_polls) g_first = GetTickCount64();
  const int n = g_next(out, 8192);
  ++g_polls;
  CHECK(n >= 0);
  return n > 0 ? out : L"";
}

// The next command, once the pipe thread has queued it.
static std::wstring NextWait() {
  for (int k = 0; k < 2000; ++k) {
    std::wstring line = Next();
    if (!line.empty()) return line;
    Sleep(1);
  }
  return L"<none>";
}

static HANDLE Connect() {
  for (int k = 0; k < 300; ++k) {  // the pipe thread starts after DllMain
    const HANDLE h = CreateFileW(g_pipe.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, OPEN_EXISTING, 0, nullptr);
    if (h != INVALID_HANDLE_VALUE) return h;
    Sleep(10);
  }
  return INVALID_HANDLE_VALUE;
}

static void Send(HANDLE h, const std::string& line) {
  DWORD put = 0;
  CHECK(WriteFile(h, line.data(), DWORD(line.size()), &put, nullptr) && put == line.size());
}

static std::string g_rest;  // bytes after the last line read

static std::string ReadLine(HANDLE h, DWORD ms = 2000) {
  const ULONGLONG until = GetTickCount64() + ms;
  for (;;) {
    if (const size_t nl = g_rest.find('\n'); nl != std::string::npos) {
      std::string line = g_rest.substr(0, nl);
      g_rest.erase(0, nl + 1);
      return line;
    }
    DWORD avail = 0;
    if (!PeekNamedPipe(h, nullptr, 0, nullptr, &avail, nullptr)) return "<broken>";
    if (avail) {
      static char buf[1 << 17];
      DWORD got = 0;
      ReadFile(h, buf, min(avail, DWORD(sizeof buf)), &got, nullptr);
      g_rest.append(buf, got);
    } else if (GetTickCount64() > until) {
      return "<timeout>";
    } else {
      Sleep(1);
    }
  }
}

static bool Has(const std::string& s, const char* part) { return s.find(part) != std::string::npos; }

// A request the script must get as `script`, answered with (ok, text, data), whose reply line must be `reply`.
static void RoundTrip(HANDLE h, const std::string& request, const std::wstring& script, bool ok, const char* text,
                      const char* data, const std::string& reply) {
  static int handle = 0;
  Send(h, request + "\n");
  const std::wstring line = NextWait();
  CHECK(line == std::to_wstring(++handle) + L" " + script);
  if (line != std::to_wstring(handle) + L" " + script) printf("  got  %ls\n  want %d %ls\n", line.c_str(), handle, script.c_str());
  CHECK(g_reply(handle, ok, text, data) == 0);
  const std::string got = ReadLine(h);
  CHECK(got == reply);
  if (got != reply) printf("  got  %s\n  want %s\n", got.c_str(), reply.c_str());
}

static void Refused(HANDLE h, const std::string& request, const char* reply_start, const char* part) {
  Send(h, request + "\n");
  const std::string got = ReadLine(h);
  CHECK(got.starts_with(reply_start) && Has(got, part));
  if (!got.starts_with(reply_start) || !Has(got, part)) printf("  %s\n  -> %s\n", request.substr(0, 120).c_str(), got.c_str());
}

// An exec request's reply: it never reaches the script (QUEN_Next stays ""), it runs inside QUEN_Next.
static std::string ExecReply(HANDLE h, const std::string& request) {
  Send(h, request + "\n");
  for (int k = 0; k < 2000; ++k) {
    CHECK(Next().empty());
    std::string line = ReadLine(h, 1);
    if (line != "<timeout>") return line;
  }
  return "<timeout>";
}

// QUEN_CamAt(frame) as numbers: none for "".
static std::vector<double> CamAt(int frame) {
  wchar_t out[512];
  const int n = g_camat(frame, out, 512);
  CHECK(n >= 0);
  std::vector<double> v;
  std::wistringstream in(n > 0 ? out : L"");
  for (double x; in >> x;) v.push_back(x);
  CHECK(v.empty() || v.size() == 7);
  return v;
}

// A cam.path request as quen sends it: canonical keyframes, defaults left out.
static std::string CamPath(int id, const std::string& rest) {
  return R"({"id":)" + std::to_string(id) + R"(,"ttl_ms":5000,"verb":"cam.path",)" + rest + "}";
}
constexpr const char* kInnKeys =
    R"("keys":[{"t":0.0,"pos":[100.0,200.0,10.0],"rot":[-5.0,170.0,0.0],"fov":60.0},)"
    R"({"t":2.0,"pos":[103.0,201.0,10.5],"rot":[0.0,-170.0,0.0],"fov":60.0,"ease":"ease_in_out"},)"
    R"({"t":4.0,"pos":[103.0,201.0,10.5],"rot":[10.0,-120.0,5.0],"fov":45.0},)"
    R"({"t":7.0,"pos":[98.0,205.0,11.0],"rot":[0.0,-60.0,0.0],"fov":50.0}])";

// The camera paths: cam.path's checks, and its sampling against quen-core sweep.rs's reference vectors
// (sweep::tests::reference_vectors), played as frame step at 4 fps so every vector's time is a frame.
static void Paths(HANDLE h) {
  const char* pose = "photomode 100 200 10 -5 170 0 60";
  const char* pose_json = R"({"camera":"photomode","pos":[100,200,10],"rot":[-5,170,0],"fov":60})";
  struct Vector {
    double t, v[7];
  };
  // sweep.rs's reference sweep (inn) under keys, its linear copy under constant speed eased in and out, and two().
  const Vector keys[] = {
      {0.0, {100.000000, 200.000000, 10.000000, -5.000000, 170.000000, 0.000000, 60.000000}},
      {0.5, {100.890625, 200.296875, 10.148438, -3.803810, 174.602628, 0.000000, 60.000000}},
      {1.25, {102.314453, 200.771484, 10.385742, -2.041704, 181.241670, 0.000000, 60.000000}},
      {2.0, {103.000000, 201.000000, 10.500000, 0.000000, 190.000000, 0.000000, 60.000000}},
      {2.75, {103.000000, 201.000000, 10.500000, 3.131433, 202.283564, 1.184930, 56.445209}},
      {3.5, {103.000000, 201.000000, 10.500000, 9.351144, 231.140883, 4.671936, 45.984192}},
      {4.0, {103.000000, 201.000000, 10.500000, 10.000000, 240.000000, 5.000000, 45.000000}},
      {5.5, {101.125000, 202.500000, 10.687500, 6.271576, 269.503105, 3.125000, 46.875000}},
      {7.0, {98.000000, 205.000000, 11.000000, 0.000000, 300.000000, 0.000000, 50.000000}},
  };
  const Vector constant[] = {
      {0.0, {100.000000, 200.000000, 10.000000, -5.000000, 170.000000, 0.000000, 60.000000}},
      {1.0, {100.553810, 200.184603, 10.092302, -4.214690, 173.065144, 0.000000, 60.000000}},
      {2.5, {102.913306, 200.971102, 10.485551, -0.733686, 186.699050, 0.000000, 60.000000}},
      {3.5, {102.133308, 201.693354, 10.586669, 8.377115, 258.659924, 4.133308, 45.866692}},
      {4.5, {100.410355, 203.071716, 10.758964, 4.775688, 276.274653, 2.410355, 47.589645}},
      {6.0, {98.456051, 204.635159, 10.954395, 0.848109, 294.904290, 0.456051, 49.543949}},
      {7.0, {98.000000, 205.000000, 11.000000, 0.000000, 300.000000, 0.000000, 50.000000}},
  };
  const Vector two[] = {
      {0.0, {0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 0.000000, 60.000000}},
      {1.0, {1.000000, -0.500000, 0.250000, 2.292523, 22.411368, 0.000000, 55.000000}},
      {2.0, {2.000000, -1.000000, 0.500000, 4.996722, 44.818750, 0.000000, 50.000000}},
      {3.0, {3.000000, -1.500000, 0.750000, 7.703879, 67.319774, 0.000000, 45.000000}},
      {4.0, {4.000000, -2.000000, 1.000000, 10.000000, 90.000000, 0.000000, 40.000000}},
  };
  std::string linear = kInnKeys;
  linear.erase(linear.find(R"(,"ease":"ease_in_out")"), 21);
  const struct {
    std::string request;
    const wchar_t* script;
    const Vector* vectors;
    size_t count;
  } paths[] = {
      {CamPath(40, std::string(R"("mode":"step","fps":4,)") + kInnKeys), L"cam.path 1 29", keys, std::size(keys)},
      {CamPath(41, R"("mode":"step","fps":4,"speed":"constant","ease":"ease_in_out",)" + linear), L"cam.path 1 29",
       constant, std::size(constant)},
      {CamPath(42, R"("mode":"step","fps":4,"keys":[{"t":0.0,"pos":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"fov":60.0},)"
                   R"({"t":4.0,"pos":[4.0,-2.0,1.0],"rot":[10.0,90.0,0.0],"fov":40.0}])"),
       L"cam.path 1 17", two, std::size(two)},
  };
  double worst = 0;
  for (const auto& path : paths) {
    const std::string id = path.request.substr(6, 2);
    RoundTrip(h, path.request, path.script, true, "loaded", pose,
              R"({"id":)" + id + R"(,"ok":true,"text":"loaded","data":)" + pose_json + "}");
    for (size_t k = 0; k < path.count; ++k) {
      const Vector& want = path.vectors[k];
      const std::vector<double> got = CamAt(int(want.t * 4));
      CHECK(got.size() == 7);
      for (size_t c = 0; c < got.size(); ++c) {
        double off = std::abs(got[c] - want.v[c]);
        if (c >= 3 && c <= 5) off = std::abs(std::remainder(got[c] - want.v[c], 360.0));
        worst = max(worst, off);
        if (off > 1.5e-6) printf("FAIL %s t %g channel %zu: got %.6f want %.6f\n", id.c_str(), want.t, c, got[c], want.v[c]);
        CHECK(off <= 1.5e-6);
      }
    }
  }
  printf("  reference vectors: 21 rows, worst difference %.2g\n", worst);
  // Frame step: frames 0 to 16 of the 4 s path at 4 fps; none past them, and no real-time sample.
  CHECK(CamAt(16).size() == 7 && CamAt(17).empty() && CamAt(-1).empty());

  // Real time: from the first sample, by the clock; the last pose holds after the end.
  RoundTrip(h, CamPath(43, R"("mode":"realtime","keys":[{"t":0.0,"pos":[0.0,0.0,0.0],"rot":[0.0,0.0,0.0],"fov":60.0},)"
                           R"({"t":0.5,"pos":[1.0,0.0,0.0],"rot":[0.0,0.0,0.0],"fov":60.0}])"),
            L"cam.path 0 0", true, "", "", R"({"id":43,"ok":true,"text":""})");
  CHECK(CamAt(0).empty());
  const auto x = [] {
    const std::vector<double> v = CamAt(-1);
    return v.empty() ? -1.0 : v[0];
  };
  const double x0 = x();
  Sleep(200);
  const double x1 = x();
  Sleep(500);
  const std::vector<double> end = CamAt(-1);
  CHECK(x0 >= 0 && x0 < 0.05 && x1 > 0.3 && x1 < 0.9 && end == std::vector<double>({1, 0, 0, 0, 0, 0, 60}));
  if (!(x0 < 0.05 && x1 > 0.3 && x1 < 0.9)) printf("  real time: x %g then %g\n", x0, x1);

  RoundTrip(h, R"({"id":44,"ttl_ms":5000,"verb":"cam.frame","n":12})", L"cam.frame 12", true, "frame 12", pose,
            R"({"id":44,"ok":true,"text":"frame 12","data":)" + std::string(pose_json) + "}");
  RoundTrip(h, R"({"id":45,"ttl_ms":5000,"verb":"cam.stop"})", L"cam.stop", true, "stopped", "",
            R"({"id":45,"ok":true,"text":"stopped"})");

  // The longest cam.path (protocol.rs the_longest_cam_path_fits_a_request): 24 keyframes, every number at its longest.
  std::string longest = R"({"id":18446744073709551615,"ttl_ms":4294967295,"verb":"cam.path","mode":"step","fps":240,"keys":[)";
  for (int k = 0; k < 24; ++k) {
    char t[16];
    snprintf(t, sizeof t, "%.4f", 3599.9976 + 0.0001 * k);
    for (size_t len = strlen(t); t[len - 1] == '0';) t[--len] = 0;  // serde's shortest form: 3599.998
    longest += std::string(k ? "," : "") + R"({"t":)" + (k ? t : "0.0") +
               R"(,"pos":[-99999.9999,-99999.9999,-99999.9999],"rot":[-88.9999,-179.9999,-179.9999],"fov":178.9999)" +
               (k < 23 ? R"(,"ease":"ease_in_out"})" : "}");
  }
  longest += "]}";
  CHECK(longest.size() == 3261);
  RoundTrip(h, longest, L"cam.path 1 864000", true, "", "", R"({"id":18446744073709551615,"ok":true,"text":""})");

  // cam.path's refusals (protocol.rs cam_path_refuses_what_the_game_must_not_play, and sweep.rs's checks).
  const char* refused = R"({"id":46,"ok":false,"text":"refused: invalid cam.path: )";
  const std::string two_keys = R"("keys":[{"t":0.0,"pos":[1.0,2.5,3.0],"rot":[-10.5,170.0,0.0],"fov":60.0},)"
                               R"({"t":2.5,"pos":[4.25,2.5,3.0],"rot":[-10.5,-170.0,0.0],"fov":60.0}])";
  const auto with = [&](const std::string& from, const std::string& to) {
    std::string keys = two_keys;
    CHECK(keys.find(from) != std::string::npos);
    return keys.replace(keys.find(from), from.size(), to);
  };
  const std::pair<std::string, const char*> bad[] = {
      {R"("mode":"step",)" + two_keys, "frame step takes 1 to 240"},
      {R"("mode":"step","fps":0,)" + two_keys, "fps is a whole number"},
      {R"("mode":"step","fps":241,)" + two_keys, "frame step takes 1 to 240"},
      {R"("mode":"step","fps":2.5,)" + two_keys, "fps is a whole number"},
      {R"("mode":"realtime","fps":30,)" + two_keys, "frame step takes 1 to 240"},
      {R"("mode":"slowmo",)" + two_keys, "mode is realtime or step"},
      {R"("keys":[{"t":0.0,"pos":[1.0,2.5,3.0],"rot":[0.0,0.0,0.0],"fov":60.0}],"mode":"realtime")", "1 keyframes"},
      {R"("mode":"realtime",)" + with("-170.0,0.0", "190.0,0.0"), "not canonical"},
      {R"("mode":"realtime",)" + with("[1.0,", "[1.00001,"), "not canonical"},
      {R"("mode":"realtime",)" + with("-10.5,-170.0", "90.0,-170.0"), "keyframe 1: pitch 90"},
      {R"("mode":"realtime",)" + with("-10.5,-170.0", "-10.5,-10.5"), "keyframes 0 to 1 turn 179"},
      {R"("mode":"realtime",)" + with("\"t\":2.5", "\"t\":0.0"), "keyframe 1: t 0 is not after"},
      {R"("mode":"realtime",)" + with("\"t\":0.0", "\"t\":0.5"), "keyframe 0: t 0.5: the first keyframe is at 0"},
      {R"("mode":"realtime",)" + with("\"t\":2.5", "\"t\":3600.1"), "at most 3600 s"},
      {R"("mode":"realtime",)" + with("[4.25,", "[100000.0,"), "below 100000 m"},
      {R"("mode":"realtime",)" + with("\"fov\":60.0}]", "\"fov\":0.5}]"), "fov 0.5"},
      {R"("mode":"realtime",)" + with("\"fov\":60.0}]", "\"fov\":60.0,\"ease\":\"ease_in_out\"}]"), "last keyframe"},
      {R"("mode":"realtime","speed":"constant",)" + with("\"fov\":60.0},", "\"fov\":60.0,\"ease\":\"ease_in_out\"},"),
       "under constant speed"},
      {R"("mode":"realtime","ease":"ease_in_out",)" + two_keys, "constant speed only"},
      {R"("mode":"realtime","speed":"fast",)" + two_keys, "speed is keys or constant"},
      {R"("mode":"realtime",)" + with("\"fov\":60.0}]", "\"fov\":60.0,\"roll\":0}]"), "keyframe 1: not"},
      {R"("mode":"realtime",)" + with("\"fov\":60.0}]", "\"fov\":60.0,\"ease\":\"ease_in\"}]"), "keyframe 1: not"},
      {R"("mode":"realtime",)" + with("[4.25,2.5,3.0]", "[4.25,2.5]"), "keyframe 1: not"},
      {R"("mode":"realtime",)" + with("\"t\":2.5", "\"t\":\"2.5\""), "keyframe 1: not"},
      {R"("mode":"realtime","frames":[])", "keys is an array"},
  };
  for (const auto& [rest, why] : bad) Refused(h, CamPath(46, rest), refused, why);
  std::string many = R"("mode":"realtime","keys":[)";
  for (int k = 0; k < 25; ++k)
    many += std::string(k ? "," : "") + R"({"t":)" + std::to_string(k) + R"(,"pos":[0,0,0],"rot":[0,0,0],"fov":60})";
  Refused(h, CamPath(46, many + "]"), refused, "25 keyframes");
  Refused(h, CamPath(46, R"("mode":"realtime","speed":"keys",)" + two_keys + R"(,"x":1)"), R"({"id":46,"ok":false,"text":"refused: )",
          "unknown field \\\"x\\\" for cam.path");
  // Objects nest one deep: in a keyframe's array, the whole line is malformed.
  Refused(h, CamPath(46, R"("mode":"realtime",)" + with("[4.25,2.5,3.0]", R"([{"a":1},2.5,3.0])")),
          R"({"id":0,"ok":false,"text":"refused: )", "malformed");
  Refused(h, R"({"id":47,"ttl_ms":1,"verb":"cam.frame","n":-1})", R"({"id":47,"ok":false,"text":"refused: )", "invalid cam.frame");
  Refused(h, R"({"id":47,"ttl_ms":1,"verb":"cam.frame","n":2147483648})", R"({"id":47,"ok":false,"text":"refused: )",
          "invalid cam.frame");
  Refused(h, R"({"id":47,"ttl_ms":1,"verb":"cam.stop","n":1})", R"({"id":47,"ok":false,"text":"refused: )", "unknown field");
  CHECK(Next().empty());  // none reached the script
}

// The photo verbs (from 0.1.9): protocol.rs's wire shapes to the script line, the read-back to JSON, the refusals.
static void Photo(HANDLE h) {
  // QUENRPC_PhotoState: time, 8 grading, 8 grading as photo mode opened, rain, snow, sky flag | weather | env | world's.
  const char* state = "720.0000 0.5000 1.0000 1.0000 1.0000 6550.0000 0.0000 0.0010 0.0000 0.0000 1.0000 1.0000 1.0000 "
                      "6550.0000 0.0000 0.0000 0.0000 0.0000 0.0000 1|WT_Clear|environment\\definitions\\x y.env|WT_Snow";
  const std::string json =
      R"({"weather":"WT_Clear","minute":720,"env":"environment\\definitions\\x y.env",)"
      R"("grading":{"exposure":0.5,"contrast":1,"highlights":1,"saturation":1,"temperature":6550,"vignette":0,"grain":0.001,"chromatic_aberration":0},)"
      R"("grading_at_open":{"exposure":0,"contrast":1,"highlights":1,"saturation":1,"temperature":6550,"vignette":0,"grain":0,"chromatic_aberration":0},)"
      R"("rain":0,"snow":0,"sky_clear":true,"world_weather":"WT_Snow"})";
  const auto with = [&](const char* id, const char* text) {
    return std::string(R"({"id":)") + id + R"(,"ok":true,"text":")" + text + R"(","data":)" + json + "}";
  };
  RoundTrip(h, R"({"id":60,"ttl_ms":5000,"verb":"photo.mode","on":true})", L"photo.mode 1", true, "on", "",
            R"({"id":60,"ok":true,"text":"on"})");
  RoundTrip(h, R"({"id":61,"ttl_ms":5000,"verb":"photo.mode","on":false})", L"photo.mode 0", true, "off", "",
            R"({"id":61,"ok":true,"text":"off"})");
  RoundTrip(h, R"({"id":62,"ttl_ms":5000,"verb":"photo.get"})", L"photo.get", true, "got", state, with("62", "got"));
  RoundTrip(h, R"({"id":63,"ttl_ms":5000,"verb":"photo.weather","name":"WT_Clear"})", L"photo.weather WT_Clear", true,
            "set", state, with("63", "set"));
  RoundTrip(h, R"({"id":64,"ttl_ms":5000,"verb":"photo.env","path":"environment\\definitions\\x y.env"})",
            L"photo.env environment\\definitions\\x y.env", true, "set", state, with("64", "set"));
  RoundTrip(h, R"({"id":65,"ttl_ms":5000,"verb":"photo.time","minute":1439.5})", L"photo.time 1439.500000", true, "set",
            state, with("65", "set"));
  RoundTrip(h, R"({"id":66,"ttl_ms":5000,"verb":"photo.grade","exposure":0.5,"temperature":6550.0})",
            L"photo.grade 1 0.500000 0 0 0 0 0 0 1 6550.000000 0 0 0 0 0 0", true, "set", state, with("66", "set"));
  RoundTrip(h, R"({"id":67,"ttl_ms":5000,"verb":"photo.grade","chromatic_aberration":2,"grain":-1e-3})",
            L"photo.grade 0 0 0 0 0 0 0 0 0 0 0 0 1 -0.001000 1 2.000000", true, "set", state, with("67", "set"));
  RoundTrip(h, R"({"id":68,"ttl_ms":5000,"verb":"photo.reset","part":"grading"})", L"photo.reset grading", true,
            "reset", state, with("68", "reset"));
  RoundTrip(h, R"({"id":69,"ttl_ms":5000,"verb":"photo.tick","tick_s":1e-5})", L"photo.tick 1 0.000010 0 0", true,
            "called", "", R"({"id":69,"ok":true,"text":"called"})");
  RoundTrip(h, R"({"id":70,"ttl_ms":5000,"verb":"photo.tick","cloth_s":10})", L"photo.tick 0 0 1 10.000000", true,
            "called", "", R"({"id":70,"ok":true,"text":"called"})");
  RoundTrip(h, R"({"id":71,"ttl_ms":5000,"verb":"photo.list","what":"envs","filter":"x y"})", L"photo.list envs x y",
            true, "2 of 9 envs", "a\\x y.env|b\\X Y.env",
            R"({"id":71,"ok":true,"text":"2 of 9 envs","data":["a\\x y.env","b\\X Y.env"]})");
  RoundTrip(h, R"({"id":72,"ttl_ms":5000,"verb":"photo.list","what":"weathers"})", L"photo.list weathers", true,
            "0 of 0 weathers", "", R"({"id":72,"ok":true,"text":"0 of 0 weathers","data":[]})");
  // The script's refusal carries no data.
  RoundTrip(h, R"({"id":73,"ttl_ms":5000,"verb":"photo.get"})", L"photo.get", false, "photo.get needs photo mode", "",
            R"({"id":73,"ok":false,"text":"photo.get needs photo mode"})");

  const char* refused = R"({"id":74,"ok":false,"text":"refused: )";
  const std::pair<const char*, const char*> bad[] = {
      {R"({"id":74,"ttl_ms":1,"verb":"photo.mode"})", "invalid photo.mode"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.mode","on":1})", "invalid photo.mode"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.get","x":0})", "unknown field \\\"x\\\" for photo.get"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.weather","name":""})", "invalid photo.weather"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.env","path":"a\u0001b"})", "invalid photo.env"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.env","name":"x"})", "invalid photo.env"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.time","minute":1440})", "invalid photo.time"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.time","minute":-0.5})", "invalid photo.time"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.time","minute":"noon"})", "invalid photo.time"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.grade"})", "invalid photo.grade"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.grade","exposure":1e39})", "invalid photo.grade"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.grade","exposure":null})", "invalid photo.grade"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.grade","exposure":1,"dof":1})", "unknown field \\\"dof\\\" for photo.grade"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.reset"})", "invalid photo.reset"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.reset","part":"camera"})", "invalid photo.reset"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.tick"})", "invalid photo.tick"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.tick","tick_s":-1})", "invalid photo.tick"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.tick","cloth_s":10.5})", "invalid photo.tick"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.list"})", "invalid photo.list"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.list","what":"props"})", "invalid photo.list"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.list","what":"envs","filter":""})", "invalid photo.list"},
      {R"({"id":74,"ttl_ms":1,"verb":"photo.list","what":"envs","filter":"a\nb"})", "invalid photo.list"},
  };
  for (const auto& [line, why] : bad) Refused(h, line, refused, why);
  Refused(h, R"({"id":74,"ttl_ms":1,"verb":"photo.list","what":"envs","filter":")" + std::string(201, 'x') + "\"}",
          refused, "invalid photo.list");
  RoundTrip(h, R"({"id":75,"ttl_ms":5000,"verb":"photo.list","what":"envs","filter":")" + std::string(200, 'x') + "\"}",
            L"photo.list envs " + std::wstring(200, L'x'), true, "0 of 9 envs", "",
            R"({"id":75,"ok":true,"text":"0 of 9 envs","data":[]})");
  CHECK(Next().empty());  // none of the refused reached the script
}

// The scene verbs (0.1.13): each to the script line and its read-back to JSON, the refusals of protocol.rs's tests, and
// read-backs that don't parse.
static void Scene(HANDLE h) {
  RoundTrip(h, R"({"id":60,"ttl_ms":5000,"verb":"scene.frame","radius":30,"max":16,"points":3})",
            L"scene.frame 30.000000 16 3 0 0 0 0 0 0", true, "read",
            "2 1 2 3 -10 90 0 40|8191 -7 0.1 -0.2 0.05 3.5 4 5 6 90 4 5 7.6 4 5 6.9|Geralt|geralt.w2ent|"
            "8 12 0 0 0 9 1 2 3 0 0 0 0 0 0 0|Guard|",
            R"({"id":60,"ok":true,"text":"read","data":{"found":2,"camera":{"pos":[1,2,3],"rot":[-10,90,0],"fov":40},)"
            R"("entities":[{"id":-7,"name":"Geralt","entity":"geralt.w2ent","entity_cut":true,"player":true,"pos":[4,5,6],)"
            R"("heading":90,"head":[4,5,7.6],"torso":[4,5,6.9],"in_front":true,"in_view":true,"x":0.1,"y":-0.2,"share":0.05,)"
            R"("box_whole":true,"distance":3.5,"visible":{"head":true,"torso":true,"origin":true},"visible_points":3},)"
            R"({"id":12,"name":"Guard","entity":"","entity_cut":false,"player":false,"pos":[1,2,3],"heading":0,)"
            R"("head":[0,0,0],"torso":null,"in_front":false,"in_view":false,"x":null,"y":null,"share":0,"box_whole":false,)"
            R"("distance":9,)"
            R"("visible":{"head":null,"torso":null,"origin":null},"visible_points":0}]}})");
  RoundTrip(h,
            R"({"id":61,"ttl_ms":5000,"verb":"scene.frame","radius":2,"max":1,"points":0,"at":[604.87,2089.32,30.99],)"
            R"("entity_id":-123456789})",
            L"scene.frame 2.000000 1 0 1 604.869995 2089.320068 30.990000 1 -123456789", true, "read", "0 0 0 0 0 0 0 40",
            R"({"id":61,"ok":true,"text":"read","data":{"found":0,"camera":{"pos":[0,0,0],"rot":[0,0,0],"fov":40},)"
            R"("entities":[]}})");
  RoundTrip(h, R"({"id":62,"ttl_ms":5000,"verb":"scene.ground","xyz":[1,2,3,-4.5,5.5,6],"range":10,"radius":0.5,"search":0})",
            L"scene.ground 10.000000 0.500000 0.000000 1.000000 2.000000 3.000000 -4.500000 5.500000 6.000000", true, "read",
            "2|31 3.5 1 2 3.4 0 0 1 -1000 0 1.5 2 3.5|0 0 0 0 0 0 0 0 0.25 2 0 0 0",
            R"({"id":62,"ok":true,"text":"read","data":[{"nav_z":3.5,"ground":{"pos":[1,2,3.4],"normal":[0,0,1]},)"
            R"("water_level":-1000,"water_depth":0,"free":true,"safe":[1.5,2,3.5]},{"nav_z":null,"ground":null,)"
            R"("water_level":0.25,"water_depth":2,"free":null,"safe":null}]})");
  RoundTrip(h, R"({"id":63,"ttl_ms":5000,"verb":"scene.ray","segments":[0,0,1,3,0,1.5],"radius":0.3})",
            L"scene.ray 0.300000 0.000000 0.000000 1.000000 3.000000 0.000000 1.500000", true, "read",
            "1|1 1.5 0 1.25 -1 0 0 1.52",
            R"({"id":63,"ok":true,"text":"read","data":[{"hit":true,"pos":[1.5,0,1.25],"normal":[-1,0,0],"distance":1.52}]})");
  RoundTrip(h, R"({"id":64,"ttl_ms":5000,"verb":"nav.line","segments":[0,0,1,3,0,1.5],"width":0.4})",
            L"nav.line 0.400000 0.000000 0.000000 1.000000 3.000000 0.000000 1.500000", true, "read", "1|3 3 0 1.5 0.75",
            R"({"id":64,"ok":true,"text":"read","data":[{"clear":true,"clear_in_direction":true,"reach":[3,0,1.5],)"
            R"("edge_m":0.75}]})");
  RoundTrip(h, R"({"id":65,"ttl_ms":5000,"verb":"cam.noclip","on":true})", L"cam.noclip 1", true, "no-clip on", "",
            R"({"id":65,"ok":true,"text":"no-clip on"})");

  const char* refused1 = R"({"id":1,"ok":false,"text":"refused: )";
  for (const std::string& f : std::vector<std::string>{
           R"("radius":0,"max":1,"points":0)", R"("radius":100.5,"max":1,"points":0)", R"("radius":30,"max":0,"points":0)",
           R"("radius":30,"max":17,"points":0)", R"("radius":30,"max":1,"points":4)", R"("radius":30,"max":1)",
           R"("radius":30,"max":1,"points":0,"at":[1,2])", R"("radius":30,"max":1,"points":0,"at":[1000000,0,0])",
           R"("radius":30,"max":1,"points":0,"entity_id":1.5)", R"("radius":30,"max":1,"points":0,"entity_id":2147483648)"})
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"scene.frame",)" + f + "}", refused1, "invalid scene.frame");
  std::string many = "[0";
  for (int k = 1; k < 51; ++k) many += ",0";
  many += "]";
  for (const std::string& f : std::vector<std::string>{
           R"("xyz":[],"range":10,"radius":0,"search":0)", R"("xyz":[1,2],"range":10,"radius":0,"search":0)",
           R"("xyz":)" + many + R"(,"range":10,"radius":0,"search":0)", R"("xyz":[1,2,3],"range":0,"radius":0,"search":0)",
           R"("xyz":[1,2,3],"range":10,"radius":10.5,"search":0)", R"("xyz":[1,2,3],"range":10,"radius":0,"search":50.5)",
           R"("xyz":[1,2,3],"range":10,"radius":0)", R"("xyz":["1","2","3"],"range":10,"radius":0,"search":0)"})
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"scene.ground",)" + f + "}", refused1, "invalid scene.ground");
  std::string segs17 = "[0";
  for (int k = 1; k < 102; ++k) segs17 += ",0";
  segs17 += "]";
  for (const std::string& f : std::vector<std::string>{R"("segments":[0,0,0,1,1,1],"radius":2.5)",
                                                       R"("segments":[0,0,0,1,1],"radius":0)",
                                                       R"("segments":)" + segs17 + R"(,"radius":0)"})
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"scene.ray",)" + f + "}", refused1, "invalid scene.ray");
  for (const char* f : {R"("segments":[0,0,0,1,1,1],"width":5.5)", R"("segments":[0,0,0,1,1,1],"width":-1)"})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"nav.line",)") + f + "}", refused1, "invalid nav.line");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"cam.noclip","on":1})", refused1, "invalid cam.noclip");
  // exec: the function's name, its args (strings without quotes, printable ASCII, 8 at most, 100 bytes each), the line.
  const std::string arg99 = "\"" + std::string(33 * 3, 'a').replace(1, 1, " ") + "\"";
  std::string long_args = arg99;
  for (int k = 1; k < 8; ++k) long_args += "," + arg99;
  for (const std::string& f : std::vector<std::string>{
           R"("function":"")", R"("function":"a b")", R"("function":")" + std::string(65, 'n') + "\"",
           R"("line":"quit")", R"("function":"f","args":["a\"b"])", R"("function":"f","args":["a'b"])",
           R"("function":"f","args":["\u00e9"])", R"("function":"f","args":["a\tb"])",
           R"("function":"f","args":["1","2","3","4","5","6","7","8","9"])",
           R"("function":"f","args":[")" + std::string(101, 'x') + "\"]", R"("function":"f","args":[1])",
           R"("function":"f","args":null)", R"("function":"f","args":[)" + long_args + "]"})
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"exec",)" + f + "}", refused1, "invalid exec");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"exec","function":"f","args":["a",1]})", R"({"id":0,"ok":false,"text":"refused: )",
          "malformed");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"exec","function":"f","x":1})", refused1, "unknown field");
  CHECK(Next().empty());  // none of them reached the script

  // Read-backs that don't parse: a count other than the records, flags past their bits, a number short, an id that
  // isn't one.
  const std::vector<std::pair<std::string, std::vector<const char*>>> bad = {
      {R"("verb":"scene.ground","xyz":[1,2,3],"range":10,"radius":0,"search":0)",
       {"2|31 1 2 3 4 5 6 7 8 9 10 11 12", "1|32 1 2 3 4 5 6 7 8 9 10 11 12", "1|1 1 2 3 4 5 6 7 8 9 10 11", "|"}},
      {R"("verb":"scene.ray","segments":[0,0,0,1,1,1],"radius":0)", {"1|1 1 2 3 4 5 6", "1|2 1 2 3 4 5 6 7", "17"}},
      {R"("verb":"nav.line","segments":[0,0,0,1,1,1],"width":0.4)", {"1|4 1 2 3 4", "1|1 1 2 3"}},
      {R"("verb":"scene.frame","radius":30,"max":16,"points":3)",
       {"0 1 2 3 4 5 6", "1 1 2 3 4 5 6 7|1 1 0 0 0 0 0 0 0 0 0 0 0 0 0|a|b", "1 1 2 3 4 5 6 7|1 1.5 0 0 0 0 0 0 0 0 0 0 0 0 0 0|a|b",
        "1 1 2 3 4 5 6 7|8192 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0|a|b", "1 1 2 3 4 5 6 7|1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0|a"}}};
  for (const auto& [request, replies] : bad) {
    for (const char* data : replies) {
      Send(h, R"({"id":66,"ttl_ms":5000,)" + request + "}\n");
      CHECK(g_reply(_wtoi(NextWait().c_str()), true, "read", data) == 0);
      const std::string got = ReadLine(h);
      CHECK(got.starts_with(R"({"id":66,"ok":false,"text":"modQuen's read-back did not parse)"));
      if (!got.starts_with(R"({"id":66,"ok":false,"text":"modQuen's read-back did not parse)"))
        printf("  %s -> %s\n", data, got.c_str());
    }
  }
}

// The free camera (0.1.17): freecam, freecam.set and freecam.get to the script and their state back as JSON; a state
// that doesn't parse; the refusals.
static void FreeCam(HANDLE h) {
  const char* state = "1 0 661.08 1881.99 11.73 661.08 1881.99 11.73 -0.53 232.3 0 668.99 1875.87 9.94 52.3 30 23 47";
  const std::string json =
      R"({"on":true,"paused":false,"pos":[661.08,1881.99,11.73],"camera":{"pos":[661.08,1881.99,11.73],)"
      R"("rot":[-0.53,232.3,0]},"player":{"pos":[668.99,1875.87,9.94],"heading":52.3},"radius":30,"near_camera":23,)"
      R"("near_player":47})";
  RoundTrip(h, R"({"id":80,"ttl_ms":5000,"verb":"freecam","on":true})", L"freecam 1", true, "EnableFreeCamera(1) called",
            state, R"({"id":80,"ok":true,"text":"EnableFreeCamera(1) called","data":)" + json + "}");
  RoundTrip(h, R"({"id":81,"ttl_ms":5000,"verb":"freecam.set","pos":[1.5,-2.25,30],"rot":[-10.5,90,0]})",
            L"freecam.set 1.500000 -2.250000 30.000000 -10.500000 90.000000 0.000000", true, "MoveFreeCamera called", state,
            R"({"id":81,"ok":true,"text":"MoveFreeCamera called","data":)" + json + "}");
  RoundTrip(h, R"({"id":82,"ttl_ms":5000,"verb":"freecam.get","radius":100})", L"freecam.get 100.000000", true,
            "the free camera's state", "0 1 0 0 0 1 2 3 350 90 0 4 5 6 0 100 0 1",
            R"({"id":82,"ok":true,"text":"the free camera's state","data":{"on":false,"paused":true,"pos":[0,0,0],)"
            R"("camera":{"pos":[1,2,3],"rot":[350,90,0]},"player":{"pos":[4,5,6],"heading":0},"radius":100,)"
            R"("near_camera":0,"near_player":1}})");
  RoundTrip(h, R"({"id":83,"ttl_ms":5000,"verb":"freecam","on":false})", L"freecam 0", false, "no player", "",
            R"({"id":83,"ok":false,"text":"no player"})");
  // 17 numbers, an on flag of 2, a count that isn't whole.
  for (const char* bad : {"1 0 1 2 3 1 2 3 0 0 0 4 5 6 0 30 1", "2 0 1 2 3 1 2 3 0 0 0 4 5 6 0 30 1 1",
                          "1 0 1 2 3 1 2 3 0 0 0 4 5 6 0 30 1.5 1"})
    RoundTrip(h, R"({"id":84,"ttl_ms":5000,"verb":"freecam.get","radius":30})", L"freecam.get 30.000000", true, "state",
              bad, R"({"id":84,"ok":false,"text":"modQuen's read-back did not parse: )" + std::string(bad) + "\"}");

  const char* refused1 = R"({"id":1,"ok":false,"text":"refused: )";
  for (const char* f : {"", R"(,"on":1)", R"(,"on":"true")"})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"freecam")") + f + "}", refused1, "invalid freecam");
  for (const char* f : {R"(,"pos":[0,0],"rot":[0,0,0])", R"(,"pos":[0,0,0])", R"(,"pos":[1e39,0,0],"rot":[0,0,0])"})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"freecam.set")") + f + "}", refused1, "invalid freecam.set");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"freecam.set","pos":[0,0,0],"rot":[0,0,0],"fov":40})", refused1,
          "unknown field");
  for (const char* f : {"", R"(,"radius":0)", R"(,"radius":100.5)", R"(,"radius":-1)", R"(,"radius":"30")"})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"freecam.get")") + f + "}", refused1, "invalid freecam.get");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"freecam.get","radius":30,"on":true})", refused1, "unknown field");
  CHECK(Next().empty());  // none of them reached the script
}

static void Channel() {
  Load();
  CHECK(Next().empty());
  HANDLE h = Connect();
  CHECK(h != INVALID_HANDLE_VALUE);
  ULONG server = 0;
  CHECK(GetNamedPipeServerProcessId(h, &server) && server == GetCurrentProcessId());

  // The ACL: one allow entry, for this user.
  PACL dacl = nullptr;
  PSECURITY_DESCRIPTOR sd = nullptr;
  CHECK(GetSecurityInfo(h, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, nullptr, nullptr, &dacl, nullptr, &sd) == 0);
  HANDLE token;
  alignas(TOKEN_USER) BYTE user[sizeof(TOKEN_USER) + SECURITY_MAX_SID_SIZE];
  DWORD len;
  CHECK(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token) && GetTokenInformation(token, TokenUser, user, sizeof user, &len));
  ACCESS_ALLOWED_ACE* ace = nullptr;
  CHECK(dacl && dacl->AceCount == 1 && GetAce(dacl, 0, reinterpret_cast<void**>(&ace)));
  CHECK(ace && ace->Header.AceType == ACCESS_ALLOWED_ACE_TYPE
        && EqualSid(&ace->SidStart, reinterpret_cast<TOKEN_USER*>(user)->User.Sid));
  LocalFree(sd);

  // Every verb, protocol.rs's wire shapes, to the script line and back.
  RoundTrip(h, R"({"id":7,"ttl_ms":2000,"verb":"load","save":"test save 1"})", L"load test save 1", false,
            "not exactly one save matches", "", R"({"id":7,"ok":false,"text":"not exactly one save matches"})");
  RoundTrip(h, R"({"id":8,"ttl_ms":2000,"verb":"loadlast"})", L"loadlast", true, "loading the last save", "",
            R"({"id":8,"ok":true,"text":"loading the last save"})");
  RoundTrip(h, R"({"id":50,"ttl_ms":5000,"verb":"save"})", L"save", true, "save requested", "",
            R"({"id":50,"ok":true,"text":"save requested"})");
  RoundTrip(h, R"({"id":51,"ttl_ms":5000,"verb":"world.get"})", L"world.get", true, "the player's position",
            "-101.5 200.25 3 90.5 3 22 0.2500 0.0000 0 WT_Clear",
            R"({"id":51,"ok":true,"text":"the player's position","data":{"player":{"pos":[-101.5,200.25,3],"heading":90.5},)"
            R"("time":{"day":3,"hour":22},"rain":0.25,"snow":0,"sky_clear":false,"weather":"WT_Clear"}})");
  RoundTrip(h, R"({"id":52,"ttl_ms":5000,"verb":"world.get"})", L"world.get", false, "no player (not in a game session)",
            "", R"x({"id":52,"ok":false,"text":"no player (not in a game session)"})x");
  RoundTrip(h, R"x({"id":53,"ttl_ms":5000,"verb":"notice","text":"quen: you have the controls (Alt+Tab)"})x",
            L"notice 8000 quen: you have the controls (Alt+Tab)", true, "notice shown", "",
            R"({"id":53,"ok":true,"text":"notice shown"})");
  // 0.1.15: the duration before the text, notice.hide, sound.
  RoundTrip(h, R"({"id":60,"ttl_ms":5000,"verb":"notice","text":"quen: agent at work.","duration_ms":3600000})",
            L"notice 3600000 quen: agent at work.", true, "notice already shown", "",
            R"({"id":60,"ok":true,"text":"notice already shown"})");
  RoundTrip(h, R"({"id":61,"ttl_ms":5000,"verb":"notice.hide"})", L"notice.hide", true, "notice hidden", "",
            R"({"id":61,"ok":true,"text":"notice hidden"})");
  RoundTrip(h, R"({"id":62,"ttl_ms":5000,"verb":"sound","event":"gui_global_panel_open"})",
            L"sound gui_global_panel_open", true, "sound played", "", R"({"id":62,"ok":true,"text":"sound played"})");
  // items.add (0.1.10): the name last, spaces kept; the read-back on success and on nothing added.
  RoundTrip(h, R"({"id":54,"ttl_ms":5000,"verb":"items.add","name":"Red Wolf Armor 2","count":1,"equip":true})",
            L"items.add 1 1 Red Wolf Armor 2", true, "added 1 of Red Wolf Armor 2", "0 1 1",
            R"({"id":54,"ok":true,"text":"added 1 of Red Wolf Armor 2","data":{"before":0,"after":1,"equipped":true}})");
  RoundTrip(h, R"({"id":55,"ttl_ms":5000,"verb":"items.add","name":"Nope","count":1000,"equip":false})",
            L"items.add 1000 0 Nope", false, "nothing added", "2 2 0",
            R"({"id":55,"ok":false,"text":"nothing added","data":{"before":2,"after":2,"equipped":false}})");
  // world.actors (0.1.10): "<max> <radius>" to the script; per actor ten numbers and two names back, the velocity null
  // without flag 64, entity_cut with flag 128 (0.1.11), the id and actor false with flag 256 (0.1.12); no actor.
  RoundTrip(h, R"({"id":56,"ttl_ms":5000,"verb":"world.actors","radius":30,"max":20})", L"world.actors 20 30.000000",
            true, "the actors", "7|1.5 2 3 90 0.5 0 0 2.5 213 -42|Novigrad Guard|guard \"x\"|4 5 6 0 0 0 0 7 256 7||",
            R"({"id":56,"ok":true,"text":"the actors","data":{"found":7,"actors":[{"name":"Novigrad Guard",)"
            R"("entity":"guard \"x\"","pos":[1.5,2,3],"heading":90,"velocity":[0.5,0,0],"distance":2.5,"combat":true,)"
            R"("hostile":false,"guard":true,"at_work":false,"action_point":true,"scene":false,"entity_cut":true,)"
            R"("id":-42,"actor":true},{"name":"","entity":"",)"
            R"("pos":[4,5,6],"heading":0,"velocity":null,"distance":7,"combat":false,"hostile":false,"guard":false,)"
            R"("at_work":false,"action_point":false,"scene":false,"entity_cut":false,"id":7,"actor":false}]}})");
  // tag and class (0.1.12): after the numbers, '-' for the one not given.
  RoundTrip(h, R"({"id":58,"ttl_ms":5000,"verb":"world.actors","radius":8,"max":5,"tag":"q104_ciri","class":"CNewNPC"})",
            L"world.actors 5 8.000000 q104_ciri CNewNPC", true, "the actors", "0",
            R"({"id":58,"ok":true,"text":"the actors","data":{"found":0,"actors":[]}})");
  RoundTrip(h, R"({"id":59,"ttl_ms":5000,"verb":"world.actors","radius":8,"max":5,"class":"W3Boat"})",
            L"world.actors 5 8.000000 - W3Boat", true, "the actors", "0",
            R"({"id":59,"ok":true,"text":"the actors","data":{"found":0,"actors":[]}})");
  RoundTrip(h, R"({"id":57,"ttl_ms":5000,"verb":"world.actors","radius":100,"max":50})", L"world.actors 50 100.000000",
            true, "the actors", "0", R"({"id":57,"ok":true,"text":"the actors","data":{"found":0,"actors":[]}})");
  RoundTrip(h, R"({"id":9,"ttl_ms":2000,"verb":"listsaves"})", L"listsaves", true, "2 saves", "save \"one\"|C:\\two",
            R"({"id":9,"ok":true,"text":"2 saves","data":["save \"one\"","C:\\two"]})");
  RoundTrip(h, R"({"id":10,"ttl_ms":2000,"verb":"listsaves"})", L"listsaves", true, "0 saves", "",
            R"({"id":10,"ok":true,"text":"0 saves","data":[]})");
  RoundTrip(h, R"({"id":11,"ttl_ms":2000,"verb":"cam.get"})", L"cam.get", true, "", "photomode 1.5 -2.25 30.0000 0 -10.5 359 60",
            R"({"id":11,"ok":true,"text":"","data":{"camera":"photomode","pos":[1.5,-2.25,30],"rot":[0,-10.5,359],"fov":60}})");
  RoundTrip(h, R"({"id":12,"ttl_ms":2000,"verb":"cam.set","pos":[1.5,-2.25,30.0],"rot":[0.0,-10.5,359.0],"fov":60.0})",
            L"cam.set 1.500000 -2.250000 30.000000 0.000000 -10.500000 359.000000 60.000000", true, "set",
            "photomode 1.5 -2.25 30 0 -10.5 359 60",
            R"({"id":12,"ok":true,"text":"set","data":{"camera":"photomode","pos":[1.5,-2.25,30],"rot":[0,-10.5,359],"fov":60}})");
  RoundTrip(h, R"({"verb":"cam.set","rot":[0,0,90],"pos":[1,2,3],"ttl_ms":500,"id":18446744073709551615})",
            L"cam.set 1.000000 2.000000 3.000000 0.000000 0.000000 90.000000", false, "cam.set needs photo mode", "",
            R"({"id":18446744073709551615,"ok":false,"text":"cam.set needs photo mode"})");
  RoundTrip(h, R"({"id":13,"ttl_ms":2000,"verb":"cam.set","pos":[0,0,0],"rot":[0,0,0],"fov":null})",
            L"cam.set 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000", true, "", "", R"({"id":13,"ok":true,"text":""})");
  RoundTrip(h, R"({"id":14,"ttl_ms":2000,"verb":"teleport","pos":[-100.0,200.5,3.0],"rot":[0.0,0.0,90.0]})",
            L"teleport -100.000000 200.500000 3.000000 0.000000 0.000000 90.000000", true, "teleported", "",
            R"({"id":14,"ok":true,"text":"teleported"})");
  RoundTrip(h, R"({"id":15,"ttl_ms":2000,"verb":"settime","day":3,"hour":23})", L"settime 3 23", true, "", "",
            R"({"id":15,"ok":true,"text":""})");
  RoundTrip(h, R"({"id":16,"ttl_ms":2000,"verb":"weather","name":"WT_Clear"})", L"weather WT_Clear", true, "", "",
            R"({"id":16,"ok":true,"text":""})");
  RoundTrip(h, "{\"id\":1,\"ttl_ms\":2000,\"verb\":\"pause\",\"on\":false}\r", L"pause 0", false, "not implemented", "",
            R"({"id":1,"ok":false,"text":"not implemented"})");
  RoundTrip(h, R"( { "id" : 17 , "ttl_ms" : 2000 , "verb" : "pause" , "on" : true } )", L"pause 1", true, "", "",
            R"({"id":17,"ok":true,"text":""})");
  // \u escapes (a surrogate pair too) and raw UTF-8, both to the script and back; control characters in the reply text
  RoundTrip(h, R"({"id":18,"ttl_ms":2000,"verb":"load","save":"caf\u00e9 \ud83d\ude00 \/x"})",
            L"load caf\u00e9 \U0001F600 /x", true, "loading caf\xc3\xa9\n\t", "",
            R"({"id":18,"ok":true,"text":"loading caf)" "\xc3\xa9" R"(\u000a\u0009"})");
  Paths(h);
  Photo(h);
  FreeCam(h);
  Scene(h);
  // exec (0.1.13) runs only once QUEN_State showed a game: none was sent yet.
  CHECK(ExecReply(h, R"({"id":70,"ttl_ms":5000,"verb":"exec","function":"npcavoid"})") ==
        R"({"id":70,"ok":false,"text":"refused: quen's state shows no loaded game (no state yet): exec runs only with a )"
        R"(game loaded and no fade or load"})");

  // Refusals (protocol.rs refuses_unknown_verbs_fields_and_shapes, refuses_non_finite_and_out_of_range_values, more).
  const char* refused1 = R"({"id":1,"ok":false,"text":"refused: )";
  const char* refused0 = R"({"id":0,"ok":false,"text":"refused: )";
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"console","line":"quit"})", refused1, "unknown verb \\\"console\\\"");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"LOAD","save":"s"})", refused1, "unknown verb");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"load","save":"s","extra":1})", refused1, "unknown field \\\"extra\\\"");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0,0],"rot":[0,0,0],"FOV":40})", refused1, "unknown field");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"loadlast","save":"s"})", refused1, "unknown field");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"save","slot":0})", refused1, "unknown field");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"world.get","x":0})", refused1, "unknown field \\\"x\\\" for world.get");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0],"rot":[0,0,0]})", refused1, "invalid cam.set");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"teleport","pos":[0,0,0],"rot":[0,0,0],"fov":1})", refused1, "unknown field");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"load"})", refused1, "invalid load");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"notice","text":""})", refused1, "invalid notice");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"notice","text":"a\nb"})", refused1, "invalid notice");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"notice","text":")" + std::string(161, 'x') + "\"}", refused1, "invalid notice");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"notice","text":"x","ms":1})", refused1, "unknown field");
  for (const char* ms : {"0", "3600001", "1.5", "-1", "\"8\""})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"notice","text":"x","duration_ms":)") + ms + "}", refused1,
            "invalid notice");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"notice.hide","all":true})", refused1, "unknown field");
  for (const char* event : {R"("")", R"("a b")", R"("gui-x")", R"(1)"})
    Refused(h, std::string(R"({"id":1,"ttl_ms":1,"verb":"sound","event":)") + event + "}", refused1, "invalid sound");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"sound"})", refused1, "invalid sound");
  const std::vector<std::string> bad_items = {
      R"("name":"x","count":0,"equip":false)",  R"("name":"x","count":1001,"equip":false)",
      R"("name":"","count":1,"equip":false)",   R"("name":"a\tb","count":1,"equip":false)",
      R"("name":"x","count":1)",                R"("name":"x","count":1,"equip":1)",
      R"("name":")" + std::string(201, 'x') + R"(","count":1,"equip":false)"};
  for (const std::string& items : bad_items)
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"items.add",)" + items + "}", refused1, "invalid items.add");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"items.add","name":"x","count":1,"equip":false,"slot":1})", refused1,
          "unknown field");
  for (const std::string& actors : std::vector<std::string>{R"("radius":0,"max":1)", R"("radius":100.5,"max":1)", R"("radius":-1,"max":1)",
                             R"("radius":30,"max":0)", R"("radius":30,"max":51)", R"("radius":30)",
                             R"("radius":"30","max":1)", R"("max":1)", R"("radius":30,"max":1,"tag":"a b")",
                             R"("radius":30,"max":1,"tag":"")", R"("radius":30,"max":1,"class":7)",
                             R"("radius":30,"max":1,"class":")" + std::string(65, 'c') + "\""})
    Refused(h, R"({"id":1,"ttl_ms":1,"verb":"world.actors",)" + actors + "}", refused1,
            "invalid world.actors");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"world.actors","radius":30,"max":1,"slot":"x"})", refused1, "unknown field");
  Refused(h, R"({"id":1,"verb":"loadlast"})", refused1, "ttl_ms");
  Refused(h, R"({"id":-1,"ttl_ms":1,"verb":"loadlast"})", refused0, "no id");
  Refused(h, R"({"id":1.0,"ttl_ms":1,"verb":"loadlast"})", refused0, "no id");
  Refused(h, R"({"id":"1","ttl_ms":1,"verb":"loadlast"})", refused0, "no id");
  Refused(h, R"({"id":1,"ttl_ms":4294967296,"verb":"loadlast"})", refused1, "ttl_ms");
  Refused(h, R"({"id":1,"ttl_ms":1})", refused1, "verb");
  Refused(h, R"([1,2])", refused0, "malformed");
  Refused(h, "", refused0, "malformed");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"loadlast"} x)", refused0, "malformed");
  Refused(h, R"({"id":1,"id":2,"ttl_ms":1,"verb":"loadlast"})", refused0, "unique keys");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"teleport","pos":[1e39,0,0],"rot":[0,0,0]})", refused1, "invalid teleport");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"teleport","pos":[1e999,0,0],"rot":[0,0,0]})", refused1, "invalid teleport");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"cam.set","pos":[0,0,0],"rot":[0,0,0],"fov":0})", refused1, "invalid cam.set");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"settime","day":1,"hour":24})", refused1, "invalid settime");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"settime","day":2147483648,"hour":1})", refused1, "invalid settime");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"weather","name":""})", refused1, "invalid weather");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"load","save":"a\u0001b"})", refused1, "invalid load");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"pause","on":1})", refused1, "invalid pause");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"load","save":"\udc00"})", refused0, "malformed");
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"load","save":"\ud800x"})", refused0, "malformed");
  Refused(h, "{\"id\":1,\"ttl_ms\":1,\"verb\":\"load\",\"save\":\"\xff\"}", refused0, "malformed");
  Refused(h, "{\"id\":1,\"ttl_ms\":1,\"verb\":\"load\",\"save\":\"a\tb\"}", refused0, "malformed");
  Refused(h, R"({"id":01,"ttl_ms":1,"verb":"loadlast"})", refused0, "malformed");
  const std::string head = R"({"id":1,"ttl_ms":5000,"verb":"load","save":")";
  Refused(h, head + std::string(4096 - head.size() - 1, 'x') + "\"}", refused0, "message of 4097 bytes, the limit is 4096");
  CHECK(Next().empty());  // none of them reached the script

  // A request at exactly the cap is served.
  const std::string at_cap = head + std::string(4096 - head.size() - 2, 'x') + "\"}";
  CHECK(at_cap.size() == 4096);
  Send(h, at_cap + "\n");
  const std::wstring line = NextWait();
  CHECK(line.ends_with(L" load " + std::wstring(4096 - head.size() - 2, L'x')));
  CHECK(g_reply(_wtoi(line.c_str()), true, "", "") == 0);
  CHECK(ReadLine(h) == R"({"id":1,"ok":true,"text":""})");

  // ttl: an old command is answered, never run.
  Send(h, R"({"id":20,"ttl_ms":1,"verb":"loadlast"})" "\n");
  Sleep(100);
  CHECK(Next().empty());
  CHECK(ReadLine(h).starts_with(R"({"id":20,"ok":false,"text":"expired: waited )"));

  // A command the script never answers is answered at the next poll; a stale or wrong handle is ignored.
  Send(h, R"({"id":21,"ttl_ms":5000,"verb":"loadlast"})" "\n");
  const int first = _wtoi(NextWait().c_str());
  Send(h, R"({"id":22,"ttl_ms":5000,"verb":"loadlast"})" "\n");
  const int second = _wtoi(NextWait().c_str());
  CHECK(ReadLine(h) == R"x({"id":21,"ok":false,"text":"modQuen gave no reply (a script error?)"})x");
  CHECK(g_reply(first, true, "late", "") == 0);
  CHECK(g_reply(0, true, "none", "") == 0);
  CHECK(ReadLine(h, 200) == "<timeout>");
  CHECK(g_reply(second, true, "", "") == 0);
  CHECK(ReadLine(h) == R"({"id":22,"ok":true,"text":""})");

  // Read-back that doesn't parse, and a reply over 64 KiB, become refusals with the same id.
  Send(h, R"({"id":23,"ttl_ms":5000,"verb":"cam.get"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", "photomode 1 2") == 0);
  CHECK(ReadLine(h) == R"({"id":23,"ok":false,"text":"modQuen's read-back did not parse: photomode 1 2"})");
  Send(h, R"({"id":24,"ttl_ms":5000,"verb":"cam.get"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", "photomode 1 2 3 4 5 6 nan") == 0);
  CHECK(ReadLine(h).starts_with(R"({"id":24,"ok":false,"text":"modQuen's read-back did not parse)"));
  // items.add: two numbers, an equipped flag that isn't 0 or 1.
  for (const char* bad : {"1 2", "1 2 3"}) {
    Send(h, R"({"id":28,"ttl_ms":5000,"verb":"items.add","name":"x","count":1,"equip":false})" "\n");
    CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", bad) == 0);
    CHECK(ReadLine(h) == R"({"id":28,"ok":false,"text":"modQuen's read-back did not parse: )" + std::string(bad) + "\"}");
  }
  // world.actors: nine numbers (0.1.11's), a name missing, more actors than found, a flag past 511, no count, an id
  // that isn't a whole number in int's range.
  for (const char* bad : {"1|1 2 3 4 5 6 7 8 0|a|b", "1|1 2 3 4 5 6 7 8 0 1|a", "0|1 2 3 4 5 6 7 8 0 1|a|b",
                          "1|1 2 3 4 5 6 7 8 512 1|a|b", "|1 2 3 4 5 6 7 8 0 1|a|b", "1|1 2 3 4 5 6 7 8 0 1.5|a|b",
                          "1|1 2 3 4 5 6 7 8 0 2147483648|a|b"}) {
    Send(h, R"({"id":29,"ttl_ms":5000,"verb":"world.actors","radius":30,"max":20})" "\n");
    CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", bad) == 0);
    CHECK(ReadLine(h) == R"({"id":29,"ok":false,"text":"modQuen's read-back did not parse: )" + std::string(bad) + "\"}");
  }
  // world.get: 0.1.8's 7 fields, an hour past 23, a sky flag that isn't 0 or 1.
  for (const char* bad : {"1 2 3 4 5 6 WT_Clear", "1 2 3 4 5 24 0 0 1 WT_Clear", "1 2 3 4 5 6 0 0 2 WT_Clear"}) {
    Send(h, R"({"id":27,"ttl_ms":5000,"verb":"world.get"})" "\n");
    CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", bad) == 0);
    CHECK(ReadLine(h) == R"({"id":27,"ok":false,"text":"modQuen's read-back did not parse: )" + std::string(bad) + "\"}");
  }
  // The photo read-back: 19 numbers, a fifth name, a number that isn't one.
  for (const char* bad : {"720 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 0 0|WT_Clear||WT_Clear",
                          "720 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 0 0 1|WT_Clear||WT_Clear|x",
                          "720 1 1 1 1 1 1 1 1 nan 1 1 1 1 1 1 1 0 0 1|WT_Clear||WT_Clear"}) {
    Send(h, R"({"id":29,"ttl_ms":5000,"verb":"photo.get"})" "\n");
    CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", bad) == 0);
    CHECK(ReadLine(h).starts_with(R"({"id":29,"ok":false,"text":"modQuen's read-back did not parse: )"));
  }
  Send(h, R"({"id":25,"ttl_ms":5000,"verb":"listsaves"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", std::string(70000, 's').c_str()) == 0);
  CHECK(ReadLine(h).starts_with(R"({"id":25,"ok":false,"text":"reply of 70)"));
  Send(h, R"({"id":26,"ttl_ms":5000,"verb":"loadlast"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "bad \xff", "") == 0);
  CHECK(ReadLine(h) == R"({"id":26,"ok":false,"text":"modQuen's reply text is not valid UTF-8"})");

  // Queue cap: 16 wait, the 17th is refused. A reconnect drops what the old client left.
  for (int k = 0; k < 17; ++k) Send(h, "{\"id\":" + std::to_string(100 + k) + R"(,"ttl_ms":60000,"verb":"loadlast"})" "\n");
  CHECK(Has(ReadLine(h), R"({"id":116,"ok":false,"text":"refused: 16 commands already wait)"));
  CloseHandle(h);
  g_rest.clear();
  h = Connect();
  CHECK(h != INVALID_HANDLE_VALUE);
  Sleep(50);
  CHECK(Next().empty());

  // A line over the cap without its newline yet: the client is dropped; the next one is served.
  Send(h, std::string(5000, 'x'));
  CHECK(ReadLine(h) == "<broken>");
  CloseHandle(h);
  g_rest.clear();
  h = Connect();
  CHECK(h != INVALID_HANDLE_VALUE);
  Send(h, R"({"id":30,"ttl_ms":5000,"verb":"loadlast"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", "") == 0);
  CHECK(ReadLine(h) == R"({"id":30,"ok":true,"text":""})");
  CloseHandle(h);

  // Game state: a "<tick> push=N flags=F" line per QUEN_State; the first logs the polls before it.
  CHECK(g_state(0) == 0 && g_state(257) == 0 && g_state(1) == 0);
  const std::string state = ReadLog(L"W3QuenState.log");
  unsigned long long tick = 0;
  CHECK(sscanf_s(state.c_str(), "%llu push=1 flags=0\n", &tick) == 1 && tick > 0);
  CHECK(Has(state, " push=1 flags=0\n") && Has(state, " push=2 flags=257\n") && Has(state, " push=3 flags=1\n"));
  if (!Has(state, " push=3 flags=1\n")) printf("  W3QuenState.log:\n%s", state.c_str());

  const std::string log = ReadLog();
  // The first state's line gives the polls before it and the first poll's tick (taken inside that call).
  const std::string first_state = "first QUEN_State (flags 0) after " + std::to_string(g_polls) + " QUEN_Next calls, the first at tick ";
  const size_t at = log.find(first_state);
  const unsigned long long poll_tick = at == std::string::npos ? 0 : _strtoui64(log.c_str() + at + first_state.size(), nullptr, 10);
  CHECK(poll_tick >= g_first && poll_tick - g_first < 1000);
  if (!(poll_tick >= g_first && poll_tick - g_first < 1000)) printf("  W3Quen.log:\n%s", log.c_str());
  CHECK(Has(log, "natives queued=1") && Has(log, "pipe ready for pid") && Has(log, "client pid")
        && Has(log, "QUEN_Reply for handle 0") && Has(log, "over 4096 bytes: disconnecting"));
}

// ---- Tracy (trace.h) ----
// g_image (.bss: no file bytes) spans every hook's RVA in this exe and in tracyhost.exe. Plant writes each hook's 16 entry
// bytes there before the ASI loads, so a triggered ASI in tracyhost.exe hooks them all. CRenderFence_Signal's site gets a
// whole function, its entry bytes then the rest of `return a0 + 0x10`: called after the install, it runs through the
// ASI's wrapper, Tracy's zone code and Detours's trampoline.

struct Site {
  const char* name;
  uint32_t rva;
  const char* bytes;
};
static const Site kSites[] = {
#define W3_HOOK(NAME, RVA, BYTES, SIG, KIND) {#NAME, RVA, BYTES},
#include "hooks.inc"
#undef W3_HOOK
};
alignas(4096) static uint8_t g_image[0x2900000];
constexpr uint32_t kStamp501 = 0x6AC555A3;  // witcher3.exe 5.01's PE stamp
constexpr uint64_t kSignalAdd = 0x10;

static uint8_t* At(uint32_t rva) {
  uint8_t* p = reinterpret_cast<uint8_t*>(GetModuleHandleW(nullptr)) + rva;
  return p >= g_image && p + 64 <= g_image + sizeof g_image ? p : nullptr;
}

static uint8_t* Signal() {
  for (const Site& s : kSites)
    if (!strcmp(s.name, "CRenderFence_Signal")) return At(s.rva);
  return nullptr;
}

static void Plant() {
  for (const Site& s : kSites) {
    uint8_t* p = At(s.rva);
    if (!p) {
      printf("FAIL: %s's RVA 0x%X is outside g_image\n", s.name, s.rva);
      exit(1);
    }
    memcpy(p, s.bytes, 16);
  }
  // CRenderFence_Signal's 16 bytes end in `48 83 c1` (add rcx, imm8): imm8 0x10, mov rax,rcx, add rsp,20h, pop rdi,
  // mov rbx,[rsp+8] (its first instruction saved rbx there), ret.
  static constexpr uint8_t kRest[] = {kSignalAdd, 0x48, 0x8b, 0xc1, 0x48, 0x83, 0xc4, 0x20, 0x5f, 0x48, 0x8b, 0x5c, 0x24, 0x08, 0xc3};
  memcpy(Signal() + 16, kRest, sizeof kRest);
  DWORD old;
  CHECK(VirtualProtect(Signal(), 64, PAGE_EXECUTE_READWRITE, &old));
}

static uint64_t CallSignal(uint64_t x) { return reinterpret_cast<uint64_t (*)(uint64_t)>(Signal())(x); }

// Hook sites whose bytes are not as planted.
static int Changed() {
  int n = 0;
  for (const Site& s : kSites) n += memcmp(At(s.rva), s.bytes, 16) != 0;
  return n;
}

// This process's threads whose description starts with "Tracy" (Tracy names its own: SetThreadDescription).
static int TracyThreads() {
  int n = 0;
  const HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
  THREADENTRY32 te{sizeof te};
  for (BOOL more = Thread32First(snap, &te); more; more = Thread32Next(snap, &te)) {
    if (te.th32OwnerProcessID != GetCurrentProcessId()) continue;
    if (const HANDLE t = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, FALSE, te.th32ThreadID)) {
      PWSTR d = nullptr;
      if (SUCCEEDED(GetThreadDescription(t, &d)) && d) {
        n += wcsncmp(d, L"Tracy", 5) == 0;
        LocalFree(d);
      }
      CloseHandle(t);
    }
  }
  CloseHandle(snap);
  return n;
}

// TCP listeners and UDP endpoints, IPv4 and IPv6, as "tcp4 127.0.0.1:8086": this process's (mine), or any process's
// local ports. iphlpapi loads only here: the dormant modes call it after checking that no Winsock is loaded.
static std::vector<std::string> Endpoints(bool mine) {
  const HMODULE ip = LoadLibraryW(L"iphlpapi.dll");
  using Tcp = DWORD(WINAPI*)(PVOID, PDWORD, BOOL, ULONG, TCP_TABLE_CLASS, ULONG);
  using Udp = DWORD(WINAPI*)(PVOID, PDWORD, BOOL, ULONG, UDP_TABLE_CLASS, ULONG);
  const auto tcp = reinterpret_cast<Tcp>(GetProcAddress(ip, "GetExtendedTcpTable"));
  const auto udp = reinterpret_cast<Udp>(GetProcAddress(ip, "GetExtendedUdpTable"));
  const auto table = [](auto get, auto cls, ULONG af) {
    std::vector<char> buf(1 << 20);
    DWORD size = DWORD(buf.size());
    if (get(buf.data(), &size, FALSE, af, cls, 0) != NO_ERROR) buf.clear();
    return buf;
  };
  std::vector<std::string> out;
  const auto add = [&](const char* kind, DWORD pid, std::string addr, DWORD port) {
    if (!mine || pid == GetCurrentProcessId())
      out.push_back(std::string(kind) + " " + addr + ":" + std::to_string(((port & 0xff) << 8) | ((port >> 8) & 0xff)));
  };
  const auto v4 = [](DWORD a) {
    const auto* b = reinterpret_cast<const uint8_t*>(&a);
    return std::to_string(b[0]) + "." + std::to_string(b[1]) + "." + std::to_string(b[2]) + "." + std::to_string(b[3]);
  };
  const TCP_TABLE_CLASS tcp_class = mine ? TCP_TABLE_OWNER_PID_LISTENER : TCP_TABLE_OWNER_PID_ALL;
  if (auto b = table(tcp, tcp_class, AF_INET); !b.empty())
    for (const auto* t = reinterpret_cast<MIB_TCPTABLE_OWNER_PID*>(b.data()); const auto& r : std::span(t->table, t->dwNumEntries))
      add("tcp4", r.dwOwningPid, v4(r.dwLocalAddr), r.dwLocalPort);
  if (auto b = table(tcp, tcp_class, AF_INET6); !b.empty())
    for (const auto* t = reinterpret_cast<MIB_TCP6TABLE_OWNER_PID*>(b.data()); const auto& r : std::span(t->table, t->dwNumEntries))
      add("tcp6", r.dwOwningPid, "v6", r.dwLocalPort);
  if (auto b = table(udp, UDP_TABLE_OWNER_PID, AF_INET); !b.empty())
    for (const auto* t = reinterpret_cast<MIB_UDPTABLE_OWNER_PID*>(b.data()); const auto& r : std::span(t->table, t->dwNumEntries))
      add("udp4", r.dwOwningPid, v4(r.dwLocalAddr), r.dwLocalPort);
  if (auto b = table(udp, UDP_TABLE_OWNER_PID, AF_INET6); !b.empty())
    for (const auto* t = reinterpret_cast<MIB_UDP6TABLE_OWNER_PID*>(b.data()); const auto& r : std::span(t->table, t->dwNumEntries))
      add("udp6", r.dwOwningPid, "v6", r.dwLocalPort);
  return out;
}

// A local port no endpoint uses now, for TRACY_PORT.
static int FreePort() {
  const std::vector<std::string> used = Endpoints(false);
  for (int port = 49800; port < 50800; ++port) {
    const std::string tail = ":" + std::to_string(port);
    if (std::none_of(used.begin(), used.end(), [&](const std::string& e) { return e.ends_with(tail); })) return port;
  }
  return 0;
}

static std::string Joined(const std::vector<std::string>& v) {
  std::string s;
  for (const std::string& e : v) s += (s.empty() ? "" : ", ") + e;
  return s;
}

// Waits up to ms for `part` in W3Quen.log.
static bool LogShows(const char* part, DWORD ms) {
  for (const ULONGLONG until = GetTickCount64() + ms; GetTickCount64() < until; Sleep(20))
    if (Has(ReadLog(), part)) return true;
  return Has(ReadLog(), part);
}

// The dormancy checks once the ASI is loaded (Load). `triggered`: the trigger was set (the stamp gate refused), so the
// ASI's private heap may exist; else none may be new beyond `heaps`, the count before Load.
static void Dormant(bool triggered, DWORD heaps) {
  Sleep(500);  // anything the ASI would start has started
  CHECK(Changed() == 0);
  CHECK(TracyThreads() == 0);
  CHECK(GetModuleHandleW(L"ws2_32.dll") == nullptr);
  CHECK(triggered || GetProcessHeaps(0, nullptr) == heaps);
  CHECK(CallSignal(5) == 5 + kSignalAdd);
  const std::vector<std::string> mine = Endpoints(true);
  CHECK(mine.empty());
  if (!mine.empty()) printf("  endpoints: %s\n", Joined(mine).c_str());
}

// Starts `line` without a window, its output into `log`; the process, or nullptr.
static HANDLE Spawn(std::wstring line, const std::wstring& log) {
  SECURITY_ATTRIBUTES sa{sizeof sa, nullptr, TRUE};
  const HANDLE out = CreateFileW(log.c_str(), GENERIC_WRITE, FILE_SHARE_READ, &sa, CREATE_ALWAYS, 0, nullptr);
  STARTUPINFOW si{sizeof si};
  si.dwFlags = STARTF_USESTDHANDLES;
  si.hStdOutput = si.hStdError = out;
  PROCESS_INFORMATION pi{};
  const bool made = CreateProcessW(nullptr, line.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi);
  CloseHandle(out);
  if (!made) return nullptr;
  CloseHandle(pi.hThread);
  return pi.hProcess;
}

// Its exit code, or -1 if it still runs after ms (then it is killed).
static DWORD Finish(HANDLE process, DWORD ms) {
  DWORD code = DWORD(-1);
  if (!process) return code;
  if (WaitForSingleObject(process, ms) == WAIT_OBJECT_0) GetExitCodeProcess(process, &code);
  else TerminateProcess(process, 1);
  CloseHandle(process);
  return code;
}

static void TracyMode(const std::wstring& mode, const std::wstring& token, const std::wstring& pid) {
  Plant();
  const bool on = mode == L"tracy-on" || mode == L"tracy-capture";
  const bool trigger = on || mode == L"tracy-stamp" || mode == L"tracy-notoken";
  const bool with_token = mode != L"tracy-none" && mode != L"tracy-notoken";
  SetEnvironmentVariableW(L"W3_QUEN_TRACY", trigger ? L"1" : nullptr);
  SetEnvironmentVariableW(L"W3_QUEN_TOKEN", with_token ? token.c_str() : nullptr);
  SetEnvironmentVariableW(L"W3_QUEN_PID", with_token ? pid.c_str() : nullptr);
  const int port = on ? FreePort() : 0;
  if (on) {
    CHECK(port != 0);
    SetEnvironmentVariableW(L"TRACY_PORT", std::to_wstring(port).c_str());
  }
  WIN32_FILE_ATTRIBUTE_DATA before{}, after{};
  GetFileAttributesExW(Here(L"W3Quen.log").c_str(), GetFileExInfoStandard, &before);
  const DWORD heaps = GetProcessHeaps(0, nullptr);
  Load();
  if (mode == L"tracy-stamp") {
    CHECK(LogShows("tracy: REFUSED: the exe's TimeDateStamp", 2000));
    Dormant(true, heaps);
    CHECK(!Has(ReadLog(), "hooks installed") && !Has(ReadLog(), "tracy: started"));
    return;
  }
  if (!on) {
    Dormant(false, heaps);
    if (with_token) {
      CHECK(LogShows("pipe ready for pid", 2000) && !Has(ReadLog(), "tracy:"));
    } else {  // no token: no log written at all, as in 0.1.2
      GetFileAttributesExW(Here(L"W3Quen.log").c_str(), GetFileExInfoStandard, &after);
      CHECK(CompareFileTime(&before.ftLastWriteTime, &after.ftLastWriteTime) == 0);
    }
    return;
  }
  // The positive control of each dormancy check. (iphlpapi, which picked the port, may have loaded Winsock already.)
  const std::string installed = std::to_string(std::size(kSites)) + " of " + std::to_string(std::size(kSites)) + " hooks installed";
  CHECK(LogShows(installed.c_str(), 2000));
  CHECK(LogShows("tracy: started", 3000));
  CHECK(Changed() == int(std::size(kSites)));
  for (const Site& s : kSites) CHECK(At(s.rva)[0] == 0xE9);
  CHECK(GetProcessHeaps(0, nullptr) > heaps);
  for (uint64_t x = 0; x < 1000; ++x) CHECK(CallSignal(x) == x + kSignalAdd);
  // Tracy's worker, "Tracy Profiler", and no symbol or compression thread; listening on 127.0.0.1:TRACY_PORT only: no
  // other address, no IPv6, no UDP broadcast.
  const std::vector<std::string> want{"tcp4 127.0.0.1:" + std::to_string(port)};
  std::vector<std::string> mine;
  for (int k = 0; k < 150 && (TracyThreads() != 1 || (mine = Endpoints(true)) != want); ++k) Sleep(20);
  CHECK(TracyThreads() == 1);
  CHECK(mine == want);
  if (mine != want) printf("  endpoints: %s\n", Joined(mine).c_str());
  if (mode != L"tracy-capture") return;
  // A real capture: tracy-capture 0.14.1 connects, the ASI logs it, and hooked calls are recorded as zones.
  wchar_t tool[MAX_PATH]{};
  CHECK(GetEnvironmentVariableW(L"TRACY_CAPTURE", tool, MAX_PATH) > 0);
  const std::wstring capture = Here(L"tracyhost.tracy");
  const HANDLE run = Spawn(L"\"" + std::wstring(tool) + L"\" -a 127.0.0.1 -p " + std::to_wstring(port) + L" -f -s 3 -o \"" +
                               capture + L"\"",
                           Here(L"tracy-capture.log"));
  CHECK(run != nullptr);
  CHECK(LogShows("tracy: a capture connected", 10000));
  for (uint64_t x = 0; x < 100; ++x) CHECK(CallSignal(x) == x + kSignalAdd);
  CHECK(Finish(run, 30000) == 0);
  WIN32_FILE_ATTRIBUTE_DATA file{};
  CHECK(GetFileAttributesExW(capture.c_str(), GetFileExInfoStandard, &file) && file.nFileSizeLow > 0);
  // tracy-csvexport beside tracy-capture, as in Tracy's release: the hooked call's zones are in the capture.
  std::wstring csvexport = tool;
  csvexport = csvexport.substr(0, csvexport.find_last_of(L'\\') + 1) + L"tracy-csvexport.exe";
  if (GetFileAttributesW(csvexport.c_str()) != INVALID_FILE_ATTRIBUTES) {
    CHECK(Finish(Spawn(L"\"" + csvexport + L"\" \"" + capture + L"\"", Here(L"tracyhost.csv")), 30000) == 0);
    CHECK(Has(ReadLog(L"tracyhost.csv"), "CRenderFence_Signal"));
  }
}

// ---- the exec verb (0.1.13) ----
// The executor's 16 entry bytes are planted at its RVA in g_image, then the rest of a stand-in: the 4-byte displacement
// of the entry's last instruction (mov rcx, [rip+0]), then add rsp, 78h; pop r14; pop rbp (undoing the entry), and a jmp
// to FakeRun with the caller's stack, rdx and r8 as they came.

#define W3_CALL(NAME, RVA, BYTES) constexpr uint32_t k##NAME##Rva = RVA; constexpr const char* k##NAME##Bytes = BYTES;
#include "hooks.inc"
#undef W3_CALL

struct EngineString {
  const char* data;
  uint32_t size, pad;
};
static std::string g_exec_line;  // the last line the stand-in got
static uint32_t g_exec_size;
static int g_exec_calls;
static bool g_out_zeroed = true;
static const char kExecLog[] = "[NpcAvoid] on";

// The executor's stand-in: "nope..." isn't found (false, out untouched); anything else is called, its log kExecLog.
static bool FakeRun(void*, const EngineString* in, EngineString* out) {
  ++g_exec_calls;
  g_exec_line.assign(in->data, in->size ? in->size - 1 : 0);
  g_exec_size = in->size;
  g_out_zeroed = g_out_zeroed && !out->data && !out->size && !out->pad;
  if (g_exec_line.starts_with("nope")) return false;
  out->data = kExecLog;
  out->size = sizeof kExecLog;
  return true;
}

static void PlantExec() {
  uint8_t* p = At(kRemoteConsole_RunCommandRva);
  CHECK(p != nullptr);
  memcpy(p, kRemoteConsole_RunCommandBytes, 16);
  uint8_t rest[] = {0, 0, 0, 0, 0x48, 0x83, 0xc4, 0x78, 0x41, 0x5e, 0x5d, 0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xe0};
  const uint64_t target = reinterpret_cast<uint64_t>(&FakeRun);
  memcpy(rest + 13, &target, 8);
  memcpy(p + 16, rest, sizeof rest);
  DWORD old;
  CHECK(VirtualProtect(p, 64, PAGE_EXECUTE_READWRITE, &old));
}

static void ExecMode() {
  const auto* base = reinterpret_cast<const uint8_t*>(GetModuleHandleW(nullptr));
  const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS64*>(base + reinterpret_cast<const IMAGE_DOS_HEADER*>(base)->e_lfanew);
  const bool host = nt->FileHeader.TimeDateStamp == kStamp501;
  PlantExec();
  Load();
  const HANDLE h = Connect();
  CHECK(h != INVALID_HANDLE_VALUE);
  CHECK(g_state(1) == 0);
  const std::string npcavoid = R"({"id":1,"ttl_ms":5000,"verb":"exec","function":"npcavoid"})";
  if (!host) {
    const std::string got = ExecReply(h, npcavoid);
    CHECK(got.starts_with(R"({"id":1,"ok":false,"text":"refused: the exe's PE stamp is 0x)") && Has(got, "not witcher3.exe 5.01's"));
    if (!Has(got, "PE stamp")) printf("  %s\n", got.c_str());
    CHECK(g_exec_calls == 0);
    return;
  }
  std::string got = ExecReply(h, npcavoid);
  CHECK(got == R"x({"id":1,"ok":true,"text":"called npcavoid()","data":{"line":"npcavoid()","log":"[NpcAvoid] on"}})x");
  if (!Has(got, "called npcavoid()")) printf("  %s\n", got.c_str());
  CHECK(g_exec_line == "npcavoid()" && g_exec_size == 11);
  // The line as protocol.rs's exec_line builds it: bare where the tokenizer reads it whole, else in ".
  got = ExecReply(h, R"({"id":2,"ttl_ms":5000,"verb":"exec","function":"addset","args":["EIST_Wolf","true","a b","","1.5e-3"]})");
  CHECK(got == R"x({"id":2,"ok":true,"text":"called addset(EIST_Wolf, true, \"a b\", \"\", 1.5e-3)","data":)x"
               R"x({"line":"addset(EIST_Wolf, true, \"a b\", \"\", 1.5e-3)","log":"[NpcAvoid] on"}})x");
  if (!Has(got, "addset")) printf("  %s\n", got.c_str());
  CHECK(g_exec_line == R"(addset(EIST_Wolf, true, "a b", "", 1.5e-3))");
  got = ExecReply(h, R"({"id":3,"ttl_ms":5000,"verb":"exec","function":"nope","args":[]})");
  CHECK(got.starts_with(R"({"id":3,"ok":false,"text":"the game called nothing: no exec function of that name)") &&
        got.ends_with(R"x("data":{"line":"nope()","log":""}})x"));
  CHECK(g_exec_calls == 3 && g_out_zeroed);
  // quen's state gate: a fade or no game.
  for (const int flags : {257, 0}) {
    CHECK(g_state(flags) == 0);
    got = ExecReply(h, npcavoid);
    CHECK(got == R"({"id":1,"ok":false,"text":"refused: quen's state shows no loaded game (flags )" + std::to_string(flags) +
                     R"(): exec runs only with a game loaded and no fade or load"})");
  }
  CHECK(g_state(1) == 0);
  // Another mod's patch at the executor: refused with the live bytes, nothing called.
  uint8_t* entry = At(kRemoteConsole_RunCommandRva);
  entry[0] = 0xe9;
  got = ExecReply(h, npcavoid);
  CHECK(Has(got, R"("ok":false,"text":"refused: the console executor's 16 entry bytes (RVA 0x24DD370) are e9 8b c4 55)"));
  if (!Has(got, "entry bytes")) printf("  %s\n", got.c_str());
  entry[0] = 0x48;
  CHECK(ExecReply(h, npcavoid).starts_with(R"({"id":1,"ok":true,)"));
  CHECK(g_exec_calls == 4);
  const std::string log = ReadLog();
  CHECK(Has(log, "exec npcavoid(): called") && Has(log, "exec nope(): not called") && Has(log, "exec refused: the console"));
  CloseHandle(h);
}

int wmain(int argc, wchar_t** argv) {
  const std::wstring mode = argc > 1 ? argv[1] : L"";
  const std::wstring token = L"quentest-" + std::to_wstring(GetCurrentProcessId()) + L"-0123456789";
  g_pipe = L"\\\\.\\pipe\\quen-w3-" + token;
  const std::wstring pid = std::to_wstring(GetCurrentProcessId());

  if (mode == L"channel") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", token.c_str());
    SetEnvironmentVariableW(L"W3_QUEN_PID", pid.c_str());
    Channel();
  } else if (mode == L"notoken") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", nullptr);
    WIN32_FILE_ATTRIBUTE_DATA before[2]{}, after[2]{};
    const wchar_t* logs[] = {L"W3Quen.log", L"W3QuenState.log"};
    for (int k = 0; k < 2; ++k) GetFileAttributesExW(Here(logs[k]).c_str(), GetFileExInfoStandard, &before[k]);
    Load();
    Sleep(300);
    CHECK(Next().empty());
    CHECK(g_state(257) == 0);
    CHECK(CreateFileW(g_pipe.c_str(), GENERIC_READ, 0, nullptr, OPEN_EXISTING, 0, nullptr) == INVALID_HANDLE_VALUE);
    for (int k = 0; k < 2; ++k) {
      GetFileAttributesExW(Here(logs[k]).c_str(), GetFileExInfoStandard, &after[k]);
      CHECK(CompareFileTime(&before[k].ftLastWriteTime, &after[k].ftLastWriteTime) == 0);
    }
  } else if (mode == L"badtoken") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", L"..\\..\\quentest-0123456789");
    SetEnvironmentVariableW(L"W3_QUEN_PID", pid.c_str());
    Load();
    Sleep(300);
    CHECK(Next().empty());
    CHECK(Has(ReadLog(), "W3_QUEN_TOKEN is not 16-64 characters"));
  } else if (mode == L"wrongpid") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", token.c_str());
    SetEnvironmentVariableW(L"W3_QUEN_PID", std::to_wstring(GetCurrentProcessId() + 4).c_str());
    Load();
    const HANDLE h = Connect();
    CHECK(h != INVALID_HANDLE_VALUE);
    CHECK(ReadLine(h) == "<broken>");
    CHECK(Has(ReadLog(), ("refused a client: pid " + std::to_string(GetCurrentProcessId())).c_str()));
  } else if (mode == L"squat") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", token.c_str());
    SetEnvironmentVariableW(L"W3_QUEN_PID", pid.c_str());
    const HANDLE squatter = CreateNamedPipeW(g_pipe.c_str(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
                                             4096, 4096, 0, nullptr);
    CHECK(squatter != INVALID_HANDLE_VALUE);
    Load();
    Sleep(300);
    CHECK(Has(ReadLog(), "CreateNamedPipe failed, error 5"));
    CloseHandle(squatter);
  } else if (mode == L"mkhost") {
    std::ifstream in(Here(L"test.exe"), std::ios::binary);
    std::string pe((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    int32_t lfanew = 0;
    CHECK(pe.size() > 0x40);
    memcpy(&lfanew, pe.data() + 0x3c, 4);
    CHECK(lfanew > 0 && size_t(lfanew) + 12 < pe.size());
    memcpy(pe.data() + lfanew + 8, &kStamp501, 4);  // IMAGE_FILE_HEADER.TimeDateStamp
    std::ofstream(Here(L"tracyhost.exe"), std::ios::binary) << pe;
  } else if (mode.starts_with(L"tracy-")) {
    TracyMode(mode, token, pid);
  } else if (mode == L"exec") {
    SetEnvironmentVariableW(L"W3_QUEN_TOKEN", token.c_str());
    SetEnvironmentVariableW(L"W3_QUEN_PID", pid.c_str());
    ExecMode();
  } else {
    printf("usage: test.exe channel|notoken|badtoken|wrongpid|squat|tracy-stamp|mkhost|exec; tracyhost.exe "
           "tracy-none|tracy-token|tracy-notoken|tracy-on|tracy-capture|exec\n");
    return 2;
  }
  printf("%ls: %s\n", mode.c_str(), g_fail ? "FAILED" : "passed");
  return g_fail ? 1 : 0;
}
