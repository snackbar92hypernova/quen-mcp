// Offline test of W3Quen.asi (no game): out\test\test.exe <mode> loads out\test\W3Quen.asi next to the stand-in
// WitcherNativeBridge.asi (wnb_stub.cpp) and plays both ends: quen's pipe client and the script calling the natives.
//   channel   token + this pid: every verb to the script line and back, the refusals of protocol.rs's tests, ttl, queue
//             cap, a missing reply, a stale handle, read-back data, an oversize reply, reconnects, server pid, the ACL;
//             QUEN_State lines in W3QuenState.log and the poll count before the first; camera paths: cam.path's checks
//             and QUEN_CamAt against quen-core sweep.rs's reference vectors
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
            "-101.5 200.25 3 90.5 3 22 WT_Clear",
            R"({"id":51,"ok":true,"text":"the player's position","data":{"player":{"pos":[-101.5,200.25,3],"heading":90.5},)"
            R"("time":{"day":3,"hour":22},"weather":"WT_Clear"}})");
  RoundTrip(h, R"({"id":52,"ttl_ms":5000,"verb":"world.get"})", L"world.get", false, "no player (not in a game session)",
            "", R"x({"id":52,"ok":false,"text":"no player (not in a game session)"})x");
  RoundTrip(h, R"x({"id":53,"ttl_ms":5000,"verb":"notice","text":"quen: you have the controls (Alt+Tab)"})x",
            L"notice quen: you have the controls (Alt+Tab)", true, "notice shown", "",
            R"({"id":53,"ok":true,"text":"notice shown"})");
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

  // Refusals (protocol.rs refuses_unknown_verbs_fields_and_shapes, refuses_non_finite_and_out_of_range_values, more).
  const char* refused1 = R"({"id":1,"ok":false,"text":"refused: )";
  const char* refused0 = R"({"id":0,"ok":false,"text":"refused: )";
  Refused(h, R"({"id":1,"ttl_ms":1,"verb":"exec","line":"quit"})", refused1, "unknown verb \\\"exec\\\"");
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
  Send(h, R"({"id":27,"ttl_ms":5000,"verb":"world.get"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", "1 2 3 4 5 6") == 0);
  CHECK(ReadLine(h) == R"({"id":27,"ok":false,"text":"modQuen's read-back did not parse: 1 2 3 4 5 6"})");
  Send(h, R"({"id":28,"ttl_ms":5000,"verb":"world.get"})" "\n");
  CHECK(g_reply(_wtoi(NextWait().c_str()), true, "", "1 2 3 4 5 24 WT_Clear") == 0);
  CHECK(ReadLine(h) == R"({"id":28,"ok":false,"text":"modQuen's read-back did not parse: 1 2 3 4 5 24 WT_Clear"})");
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
  } else {
    printf("usage: test.exe channel|notoken|badtoken|wrongpid|squat|tracy-stamp|mkhost; tracyhost.exe "
           "tracy-none|tracy-token|tracy-notoken|tracy-on|tracy-capture\n");
    return 2;
  }
  printf("%ls: %s\n", mode.c_str(), g_fail ? "FAILED" : "passed");
  return g_fail ? 1 : 0;
}
