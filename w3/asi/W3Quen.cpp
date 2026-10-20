// W3Quen.asi, the native half of quen's Witcher 3 package (quen-mcp w3\README.md):
// registers the script natives QUEN_Next, QUEN_Reply, QUEN_State and QUEN_CamAt for modQuen through the Witcher Native
// Bridge on every game build, so the script always compiles. It writes files only when quen launched the game with
// W3_QUEN_TOKEN in its environment: W3Quen.log, and W3QuenState.log with the game state modQuen sends. It serves a pipe
// only with a valid W3_QUEN_TOKEN and W3_QUEN_PID (quen's own pid): \\.\pipe\quen-w3-<token>, JSON lines as in
// quen-core's protocol.rs, one client at a time, that pid and the current user only. Otherwise QUEN_Next always returns
// "". Outside a tracy launch it reads no exe address: the WNB API version is the only gate. In a tracy launch (quen sets
// W3_QUEN_TRACY=1 beside the token) it also hooks witcher3.exe 5.01's functions for Tracy (trace.h).
// The static import of WitcherNativeBridge.asi makes Windows load and initialise it before this module.
#define NOMINMAX
#include <windows.h>
#include <sddl.h>
#include <algorithm>
#include <array>
#include <atomic>
#include <charconv>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cwctype>
#include <deque>
#include <map>
#include <memory>
#include <mutex>
#include <share.h>
#include <string>
#include <string_view>
#include <vector>
#include "WitcherNativeBridgeApi.h"
#pragma comment(lib, "advapi32")

// quen-w3's release, named on W3Quen.log's natives line (in a tracy launch the hook lines come before it) and by pack.py's
// zip. Each release's ASI then differs from the last, which quen's doctor needs: it tells a release by its ASI's sha256.
constexpr char kVersion[] = "0.1.11";

// The caps of quen-core protocol.rs (MAX_REQUEST_BYTES, MAX_REPLY_BYTES), newline excluded.
constexpr size_t kMaxRequest = 4096;
constexpr size_t kMaxReply = 64 * 1024;
constexpr size_t kMaxQueued = 16;
// protocol.rs's MAX_NOTICE_BYTES: notice's text, from quen-w3 0.1.8.
constexpr size_t kMaxNotice = 160;
// protocol.rs's MAX_FILTER_BYTES and MAX_TICK_S: photo.list's filter and photo.tick's seconds, from quen-w3 0.1.9.
constexpr size_t kMaxFilter = 200;
constexpr float kMaxTick = 10;
// protocol.rs's MAX_ITEM_BYTES and MAX_ITEM_COUNT: items.add's name and count, from quen-w3 0.1.10.
constexpr size_t kMaxItem = 200;
constexpr uint64_t kMaxItemCount = 1000;
// protocol.rs's MAX_ACTORS_RADIUS and MAX_ACTORS: world.actors' radius and max, from quen-w3 0.1.10.
constexpr float kMaxActorsRadius = 100;
constexpr uint64_t kMaxActors = 50;
// photo.grade's sliders, in modQuen's QUEN_Grade order, as protocol.rs names them.
constexpr const char* kGrade[] = {"exposure", "contrast", "highlights", "saturation", "temperature", "vignette", "grain",
                                  "chromatic_aberration"};

namespace sweep {
struct Path;
}

// A checked request, as the script gets it: "<handle> <verb><args>", args being " <number>"... then " <name>".
struct Command {
  uint64_t id = 0;
  uint32_t ttl_ms = 0;
  std::string verb;
  std::string args;
  ULONGLONG arrived = 0;
  int handle = 0;  // 0: none
  std::shared_ptr<const sweep::Path> path;  // cam.path's, checked and precomputed
};

static std::mutex g_mutex;  // guards everything below; never held across pipe I/O
static std::deque<Command> g_inbox;
static std::deque<std::string> g_outbox;  // reply lines, newline included
static Command g_current;                 // handed to the script, not yet replied to
static int g_next_handle = 1;
static uint64_t g_polls;  // QUEN_Next calls so far
static ULONGLONG g_first_poll;  // the GetTickCount64 of the first
static HANDLE g_wake;     // auto-reset: the outbox has lines
static bool g_queued;
static bool g_token;              // W3_QUEN_TOKEN is set: the two logs are written
static std::atomic<FILE*> g_log;  // W3Quen.log next to witcher3.exe
// The game thread's only (QUEN_Next, QUEN_CamAt): the path of the last cam.path handed to the script, which plays the
// one it loaded, and the performance counter at a real-time path's first sample (0: not yet).
static std::shared_ptr<const sweep::Path> g_path;
static LONGLONG g_path_start;

// name next to witcher3.exe, truncated, readable while the game runs; null on failure.
static FILE* OpenBesideExe(const wchar_t* name) {
  wchar_t path[MAX_PATH]{};
  const DWORD len = GetModuleFileNameW(nullptr, path, MAX_PATH);
  wchar_t* slash = wcsrchr(path, L'\\');
  if (!len || len >= MAX_PATH || !slash || wcscpy_s(slash + 1, MAX_PATH - (slash + 1 - path), name) != 0) return nullptr;
  return _wfsopen(path, L"w", _SH_DENYWR);
}

static void Log(const char* format, auto... args) {
  FILE* log = g_log.load();
  if (!log) return;
  _lock_file(log);  // one line per call, whatever thread
  fprintf(log, "%llu ", GetTickCount64());
  fprintf(log, format, args...);
  fputc('\n', log);
  fflush(log);
  _unlock_file(log);
}

#include "trace.h"

static bool ValidUtf8(std::string_view s) {
  return s.empty() || MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.data(), int(s.size()), nullptr, 0) > 0;
}

static std::string Quote(std::string_view s) {
  std::string out = "\"";
  for (const unsigned char c : s) {
    if (c == '"' || c == '\\') {
      out += '\\';
      out += char(c);
    } else if (c < 0x20) {
      char esc[8];
      snprintf(esc, sizeof esc, "\\u%04x", c);
      out += esc;
    } else {
      out += char(c);
    }
  }
  return out + '"';
}

// Shortest round-trip text: a JSON number for finite v.
static std::string Number(auto v) {
  char buf[64];
  return {buf, std::to_chars(buf, buf + sizeof buf, v).ptr};
}

// For the script's StringToFloat: plain digits, no exponent.
static std::string Fixed(auto v) {
  char buf[400];
  return {buf, std::to_chars(buf, buf + sizeof buf, v, std::chars_format::fixed, 6).ptr};
}

static bool ParseDouble(std::string_view t, double& out) {
  const auto [end, ec] = std::from_chars(t.data(), t.data() + t.size(), out);
  return ec == std::errc() && end == t.data() + t.size() && std::isfinite(out);
}

static bool ParseFloat(std::string_view t, float& out) {
  double d = 0;
  const bool ok = ParseDouble(t, d);
  out = float(d);
  return ok && std::isfinite(out);
}

static std::vector<std::string_view> Split(std::string_view s, char sep) {
  std::vector<std::string_view> parts;
  for (size_t at; (at = s.find(sep)) != std::string_view::npos; s.remove_prefix(at + 1)) parts.push_back(s.substr(0, at));
  parts.push_back(s);
  return parts;
}

// ---- camera paths: quen-core sweep.rs's Path, ported line for line in double precision ----
// A cam.path's keyframes are checked and precomputed once, on the pipe thread, as sweep.rs's Path::new does; the game
// thread then samples the path on every tick (QUEN_CamAt). test.cpp checks it against sweep.rs's reference vectors.

namespace sweep {

constexpr size_t kMaxKeys = 24;
constexpr double kMaxDuration = 3600, kMaxCoord = 100000, kMaxPitch = 89, kMinFov = 1, kMaxFov = 179;
constexpr uint64_t kMaxFps = 240;
constexpr int kArcSamples = 32;
constexpr double kSamePos = 1e-3, kMaxTurn = 179, kNlerp = 0.9995, kMetresPerRadian = 1;
constexpr double kPi = 3.14159265358979323846;
constexpr double kToRad = kPi / 180.0, kToDeg = 180.0 / kPi;  // Rust's to_radians and to_degrees

using V3 = std::array<double, 3>;
using Q = std::array<double, 4>;  // w, x, y, z; unit wherever used. The camera looks along +y with +z up.

struct Key {
  double t = 0, fov = 0;
  V3 pos{}, rot{};    // rot: pitch, yaw, roll
  bool ease = false;  // the segment leaving it: smoothstep, else linear
  bool operator==(const Key&) const = default;
};

struct Pose {
  V3 pos{}, rot{};
  double fov = 0;
};

static double Norm(const double* v, size_t n) {
  double s = 0;
  for (size_t c = 0; c < n; ++c) s += v[c] * v[c];
  return std::sqrt(s);
}
// On the 0.0001 grid; adding 0 turns -0 into 0.
static double Grid(double x) { return std::round(x * 10000.0) / 10000.0 + 0.0; }
static double Wrap180(double x) { return x - 360.0 * std::round(x / 360.0); }
// In (-180, 180], on the grid.
static double Angle(double x) {
  const double a = Grid(Wrap180(x));
  return a == -180.0 ? 180.0 : a;
}
static double Ease(bool smooth, double s) { return smooth ? s * s * (3.0 - 2.0 * s) : s; }
static double Hermite(double p0, double p1, double m0, double m1, double u) {
  const double u2 = u * u, u3 = u * u * u;
  return (2.0 * u3 - 3.0 * u2 + 1.0) * p0 + (u3 - 2.0 * u2 + u) * m0 + (3.0 * u2 - 2.0 * u3) * p1 + (u3 - u2) * m1;
}
template <size_t N>
static std::array<double, N> Harmonic(const std::array<double, N>& a, const std::array<double, N>& b) {
  const double la = Norm(a.data(), N), lb = Norm(b.data(), N);
  std::array<double, N> t{};
  if (la + lb == 0.0) return t;
  for (size_t c = 0; c < N; ++c) t[c] = (lb * a[c] + la * b[c]) / (la + lb);
  return t;
}
static V3 Centripetal(const V3& a, const V3& b) {
  const double la = Norm(a.data(), 3), lb = Norm(b.data(), 3);
  if (la < kSamePos || lb < kSamePos) return {};
  const double d0 = std::sqrt(la), d1 = std::sqrt(lb);
  const double wa = d1 / (d0 * (d0 + d1)), wb = d0 / (d1 * (d0 + d1));
  return {a[0] * wa + b[0] * wb, a[1] * wa + b[1] * wb, a[2] * wa + b[2] * wb};
}

static Q Look(double yaw, double pitch) {
  const double hy = yaw * kToRad / 2.0, hp = pitch * kToRad / 2.0;
  const double sy = std::sin(hy), cy = std::cos(hy), sp = std::sin(hp), cp = std::cos(hp);
  return {cy * cp, cy * sp, sy * sp, sy * cp};
}
static Q Mul(const Q& p, const Q& o) {
  const auto [a, b, c, d] = p;
  const auto [e, f, g, h] = o;
  return {a * e - b * f - c * g - d * h, a * f + b * e + c * h - d * g, a * g - b * h + c * e + d * f,
          a * h + b * g - c * f + d * e};
}
static Q Conj(const Q& q) { return {q[0], -q[1], -q[2], -q[3]}; }
static double Dot(const Q& p, const Q& o) { return p[0] * o[0] + p[1] * o[1] + p[2] * o[2] + p[3] * o[3]; }
// The half-angle rotation vector.
static V3 Log(const Q& q) {
  const double s = Norm(q.data() + 1, 3);
  if (s < 1e-12) return {q[1], q[2], q[3]};
  const double k = std::atan2(s, q[0]) / s;
  return {q[1] * k, q[2] * k, q[3] * k};
}
static Q Exp(const V3& v) {
  const double a = Norm(v.data(), 3);
  if (a < 1e-12) return {1.0, v[0], v[1], v[2]};
  const double k = std::sin(a) / a;
  return {std::cos(a), v[0] * k, v[1] * k, v[2] * k};
}
static Q Slerp(const Q& p, const Q& o, double u) {
  const double d = Dot(p, o);
  double wa = 1.0 - u, wb = u;
  if (!(d > kNlerp)) {
    const double th = std::acos(std::clamp(d, -1.0, 1.0)), s = std::sin(th);
    wa = std::sin((1.0 - u) * th) / s;
    wb = std::sin(u * th) / s;
  }
  Q q;
  for (int c = 0; c < 4; ++c) q[c] = wa * p[c] + wb * o[c];
  const double len = Norm(q.data(), 4);
  for (double& x : q) x /= len;
  return q;
}
static V3 Forward(const Q& q) {
  const auto [w, x, y, z] = q;
  return {2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)};
}

struct Path {
  std::vector<Key> keys;
  bool constant = false;  // speed constant (else keys)
  bool ease = false;      // the sweep's ease, under constant speed
  uint32_t fps = 0;       // frame step's; 0: real time
  uint32_t frames = 0;    // frame step's frame count
  std::vector<std::array<V3, 2>> pos_tan;
  std::vector<Q> quat, s_in, s_out;
  std::vector<double> yaw, roll, roll_tan, fov_tan, arc;

  double Duration() const { return keys.back().t; }

  std::pair<Pose, Q> At(double sigma) const {
    const size_t i = std::min(size_t(std::floor(sigma)), keys.size() - 2);
    const double u = sigma - double(i);
    const Key &k0 = keys[i], &k1 = keys[i + 1];
    Pose pose;
    for (int c = 0; c < 3; ++c) pose.pos[c] = Hermite(k0.pos[c], k1.pos[c], pos_tan[i][0][c], pos_tan[i][1][c], u);
    const Q q = Slerp(Slerp(quat[i], quat[i + 1], u), Slerp(s_out[i], s_in[i + 1], u), 2.0 * u * (1.0 - u));
    const V3 f = Forward(q);
    // Yaw unwrapped against a reference that runs straight between the keyframes' unwrapped yaws.
    const double reference = yaw[i] + u * (yaw[i + 1] - yaw[i]);
    pose.rot[1] = reference + Wrap180(std::atan2(-f[0], f[1]) * kToDeg - reference);
    pose.rot[0] = std::atan2(f[2], std::hypot(f[0], f[1])) * kToDeg;
    pose.rot[2] = Hermite(roll[i], roll[i + 1], roll_tan[i], roll_tan[i + 1], u);
    pose.fov = Hermite(k0.fov, k1.fov, fov_tan[i], fov_tan[i + 1], u);
    return {pose, q};
  }

  // The path parameter at t seconds: segment index plus the segment's u.
  double Sigma(double t) const {
    const double end = Duration();
    t = std::fmin(std::fmax(t, 0.0), end);
    if (!constant) {
      size_t i = 0;
      while (i + 2 < keys.size() && keys[i + 1].t <= t) ++i;
      return double(i) + Ease(keys[i].ease, (t - keys[i].t) / (keys[i + 1].t - keys[i].t));
    }
    const double target = Ease(ease, t / end) * arc.back();
    const size_t j = std::min(size_t(std::lower_bound(arc.begin() + 1, arc.end(), target) - (arc.begin() + 1)),
                              arc.size() - 2) + 1;
    const double span = arc[j] - arc[j - 1];
    const double frac = span > 0.0 ? std::clamp((target - arc[j - 1]) / span, 0.0, 1.0) : 0.0;
    return (double(j - 1) + frac) / kArcSamples;
  }

  Pose Sample(double t) const { return At(Sigma(t)).first; }
};

// How far the camera moves between two samples: travel, and turn, roll and fov change at kMetresPerRadian.
static double Step(const std::pair<Pose, Q>& a, const std::pair<Pose, Q>& b) {
  V3 d;
  for (int c = 0; c < 3; ++c) d[c] = b.first.pos[c] - a.first.pos[c];
  Q dq;
  for (int c = 0; c < 4; ++c) dq[c] = b.second[c] - a.second[c];
  const double travel = Norm(d.data(), 3), turn = 2.0 * Norm(dq.data(), 4);
  const double roll = (b.first.rot[2] - a.first.rot[2]) * kToRad, fov = (b.first.fov - a.first.fov) * kToRad;
  return std::sqrt(travel * travel + kMetresPerRadian * kMetresPerRadian * (turn * turn + roll * roll + fov * fov));
}

// sweep.rs Path::new on p's keys, speed and ease, then the canonical check of protocol.rs's Verb::check: the keyframes
// must already be canonical. Empty, or why not.
static std::string Build(Path& p) {
  const size_t n = p.keys.size();
  if (n < 2 || n > kMaxKeys) return std::to_string(n) + " keyframes: a sweep has 2 to " + std::to_string(kMaxKeys);
  std::vector<Key> keys;
  for (const Key& k : p.keys) {
    Key c = k;
    c.t = Grid(k.t);
    c.fov = Grid(k.fov);
    for (int a = 0; a < 3; ++a) {
      c.pos[a] = Grid(k.pos[a]);
      c.rot[a] = Angle(k.rot[a]);
    }
    keys.push_back(c);
  }
  for (size_t i = 0; i < n; ++i) {
    const Key& k = keys[i];
    const std::string at = "keyframe " + std::to_string(i) + ": ";
    bool finite = std::isfinite(k.t) && std::isfinite(k.fov);
    for (int a = 0; a < 3; ++a) finite = finite && std::isfinite(k.pos[a]) && std::isfinite(k.rot[a]);
    if (!finite) return at + "a number that isn't finite";
    if (i == 0 && k.t != 0.0) return at + "t " + Number(k.t) + ": the first keyframe is at 0";
    if (i > 0 && k.t <= keys[i - 1].t) return at + "t " + Number(k.t) + " is not after the previous keyframe's";
    if (k.t > kMaxDuration) return at + "t " + Number(k.t) + ": a sweep lasts at most 3600 s";
    for (const double x : k.pos)
      if (std::abs(x) >= kMaxCoord) return at + "a coordinate " + Number(x) + ": coordinates stay below 100000 m";
    if (std::abs(k.rot[0]) > kMaxPitch) return at + "pitch " + Number(k.rot[0]) + ": beyond +-89 degrees";
    if (!(k.fov >= kMinFov && k.fov <= kMaxFov)) return at + "fov " + Number(k.fov) + ": outside 1 to 179 degrees";
    if (k.ease && i == n - 1) return at + "an ease on the last keyframe";
    if (k.ease && p.constant) return at + "an ease under constant speed: set the sweep's ease instead";
  }
  if (p.ease && !p.constant) return "the sweep's ease applies to constant speed only";

  p.quat.clear();
  for (const Key& k : keys) p.quat.push_back(Look(k.rot[1], k.rot[0]));
  auto& q = p.quat;
  for (size_t i = 1; i < n; ++i) {
    if (Dot(q[i - 1], q[i]) < 0.0)
      for (double& x : q[i]) x = -x;
    if (Dot(q[i - 1], q[i]) < std::cos(kMaxTurn / 2.0 * kToRad))
      return "keyframes " + std::to_string(i - 1) + " to " + std::to_string(i) +
             " turn 179 degrees or more: add a keyframe between";
  }

  // Each channel's change into keyframe i and out of it; a missing neighbour at either end mirrors the one there.
  const auto into = [](size_t i) { return std::max<size_t>(i, 1) - 1; };
  const auto out = [n](size_t i) { return std::min(i, n - 2); };
  std::vector<V3> chord(n - 1), m(n);
  for (size_t i = 0; i + 1 < n; ++i)
    for (int c = 0; c < 3; ++c) chord[i][c] = keys[i + 1].pos[c] - keys[i].pos[c];
  for (size_t i = 0; i < n; ++i) m[i] = Centripetal(chord[into(i)], chord[out(i)]);
  p.pos_tan.assign(n - 1, {});
  for (size_t i = 0; i + 1 < n; ++i) {
    const double d = std::sqrt(Norm(chord[i].data(), 3));
    for (int c = 0; c < 3; ++c) {
      p.pos_tan[i][0][c] = m[i][c] * d;
      p.pos_tan[i][1][c] = m[i + 1][c] * d;
    }
  }

  const auto ahead = [&](size_t i) { return Log(Mul(Conj(q[i]), q[i + 1])); };
  const auto behind = [&](size_t i) {
    V3 v = Log(Mul(Conj(q[i]), q[i - 1]));
    for (double& x : v) x = -x;
    return v;
  };
  p.s_in.assign(n, {});
  p.s_out.assign(n, {});
  for (size_t i = 0; i < n; ++i) {
    const V3 b = i < n - 1 ? ahead(i) : behind(i);
    const V3 a = i > 0 ? behind(i) : b;
    const V3 tan = Harmonic(a, b);
    V3 in, leave;
    for (int c = 0; c < 3; ++c) {
      in[c] = (a[c] - tan[c]) / 2.0;
      leave[c] = (tan[c] - b[c]) / 2.0;
    }
    p.s_in[i] = Mul(q[i], Exp(in));
    p.s_out[i] = Mul(q[i], Exp(leave));
  }

  const auto unwrap = [&](int c) {
    std::vector<double> v{keys[0].rot[c]};
    for (size_t i = 1; i < n; ++i) v.push_back(v.back() + Wrap180(keys[i].rot[c] - v.back()));
    return v;
  };
  const auto tangents = [&](const std::vector<double>& v) {
    std::vector<double> t(n);
    for (size_t i = 0; i < n; ++i)
      t[i] = Harmonic<1>({v[into(i) + 1] - v[into(i)]}, {v[out(i) + 1] - v[out(i)]})[0];
    return t;
  };
  std::vector<double> fovs;
  for (const Key& k : keys) fovs.push_back(k.fov);
  p.yaw = unwrap(1);
  p.roll = unwrap(2);
  p.roll_tan = tangents(p.roll);
  p.fov_tan = tangents(fovs);
  p.arc.clear();
  if (p.constant) {
    // The arc length at every 1 / kArcSamples of the path parameter, from 0.
    p.arc.push_back(0.0);
    auto last = p.At(0.0);
    for (size_t j = 1; j <= (n - 1) * kArcSamples; ++j) {
      const auto next = p.At(double(j) / kArcSamples);
      p.arc.push_back(p.arc.back() + Step(last, next));
      last = next;
    }
  }
  if (p.fps) p.frames = uint32_t(std::floor(p.Duration() * p.fps + 1e-6)) + 1;
  return keys == p.keys ? "" : "keyframes not canonical: send Path::keys";
}

}  // namespace sweep

static std::string ReplyLine(uint64_t id, bool ok, std::string_view text, std::string_view data = {}) {
  std::string line = "{\"id\":" + std::to_string(id) + ",\"ok\":" + (ok ? "true" : "false") + ",\"text\":" + Quote(text);
  if (!data.empty()) line += ",\"data\":" + std::string(data);
  line += '}';
  if (line.size() > kMaxReply)
    return ReplyLine(id, false, "reply of " + std::to_string(line.size()) + " bytes, the limit is " + std::to_string(kMaxReply));
  return line + '\n';
}

// Callers hold g_mutex.
static void SendLocked(std::string line) {
  g_outbox.push_back(std::move(line));
  SetEvent(g_wake);
}

// ---- requests: a flat JSON object, checked as quen-core's decode_request checks it ----

struct Value {
  enum Kind { kString, kNumber, kBool, kNull, kArray } kind = kNull;
  std::string text;                // a string, or a number's JSON text
  bool flag = false;               // a bool
  std::vector<std::string> items;  // an array's numbers, or flat objects (cam.path's keys), as JSON text
};

// A request's flat object holds strings, numbers, bools, null and arrays of numbers; only an array may hold objects,
// themselves flat, and no deeper.
struct Parser {
  std::string_view s;
  size_t i = 0;
  int depth = 0;  // objects open

  void Ws() {
    while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n')) ++i;
  }
  // Skips s at i (whitespace first) if it starts with w.
  bool Word(std::string_view w) {
    Ws();
    if (s.substr(i, w.size()) != w) return false;
    i += w.size();
    return true;
  }
  bool Eat(char c) { return Word({&c, 1}); }
  bool Hex4(uint32_t& v) {
    if (s.size() - i < 4) return false;
    const auto [end, ec] = std::from_chars(s.data() + i, s.data() + i + 4, v, 16);
    if (ec != std::errc() || end != s.data() + i + 4) return false;
    i += 4;
    return true;
  }
  bool String(std::string& out) {
    if (!Eat('"')) return false;
    while (i < s.size()) {
      const unsigned char c = s[i++];
      if (c == '"') return true;
      if (c < 0x20) return false;
      if (c != '\\') {
        out += char(c);
        continue;
      }
      if (i >= s.size()) return false;
      uint32_t cp = 0;
      switch (s[i++]) {
        case '"': out += '"'; continue;
        case '\\': out += '\\'; continue;
        case '/': out += '/'; continue;
        case 'b': out += '\b'; continue;
        case 'f': out += '\f'; continue;
        case 'n': out += '\n'; continue;
        case 'r': out += '\r'; continue;
        case 't': out += '\t'; continue;
        case 'u':
          if (!Hex4(cp) || (cp >= 0xDC00 && cp < 0xE000)) return false;
          if (cp >= 0xD800 && cp < 0xDC00) {  // a high surrogate: its low half must follow
            uint32_t lo = 0;
            if (s.substr(i, 2) != "\\u") return false;
            i += 2;
            if (!Hex4(lo) || lo < 0xDC00 || lo >= 0xE000) return false;
            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
          }
          break;
        default: return false;
      }
      if (cp < 0x80) {
        out += char(cp);
      } else if (cp < 0x800) {
        out += char(0xC0 | cp >> 6);
        out += char(0x80 | (cp & 0x3F));
      } else if (cp < 0x10000) {
        out += char(0xE0 | cp >> 12);
        out += char(0x80 | (cp >> 6 & 0x3F));
        out += char(0x80 | (cp & 0x3F));
      } else {
        out += char(0xF0 | cp >> 18);
        out += char(0x80 | (cp >> 12 & 0x3F));
        out += char(0x80 | (cp >> 6 & 0x3F));
        out += char(0x80 | (cp & 0x3F));
      }
    }
    return false;
  }
  bool Num(std::string& out) {
    Ws();
    const size_t start = i;
    const auto digits = [&] {
      const size_t from = i;
      while (i < s.size() && s[i] >= '0' && s[i] <= '9') ++i;
      return i > from;
    };
    if (i < s.size() && s[i] == '-') ++i;
    if (i < s.size() && s[i] == '0') {
      ++i;
    } else if (!digits()) {
      return false;
    }
    if (i < s.size() && s[i] == '.' && (++i, !digits())) return false;
    if (i < s.size() && (s[i] == 'e' || s[i] == 'E')) {
      ++i;
      if (i < s.size() && (s[i] == '+' || s[i] == '-')) ++i;
      if (!digits()) return false;
    }
    out.assign(s.substr(start, i - start));
    return true;
  }
  bool Val(Value& v) {
    Ws();
    if (i < s.size() && s[i] == '"') {
      v.kind = Value::kString;
      return String(v.text);
    }
    if (Word("true")) {
      v.kind = Value::kBool;
      v.flag = true;
      return true;
    }
    if (Word("false")) {
      v.kind = Value::kBool;
      return true;
    }
    if (Word("null")) return true;
    if (Eat('[')) {
      v.kind = Value::kArray;
      if (Eat(']')) return true;
      do {
        Ws();
        const size_t start = i;
        std::map<std::string, Value> inner;
        if (i < s.size() && s[i] == '{') {
          if (depth > 1 || !Object(inner)) return false;
          v.items.emplace_back(s.substr(start, i - start));
        } else if (!Num(v.items.emplace_back())) {
          return false;
        }
      } while (Eat(','));
      return Eat(']');
    }
    v.kind = Value::kNumber;
    return Num(v.text);
  }
  // A JSON object with unique keys.
  bool Object(std::map<std::string, Value>& fields) {
    if (!Eat('{')) return false;
    ++depth;
    bool ok = true;
    if (!Eat('}')) {
      do {
        std::string key;
        Value v;
        ok = String(key) && Eat(':') && Val(v) && fields.emplace(std::move(key), std::move(v)).second;
      } while (ok && Eat(','));
      ok = ok && Eat('}');
    }
    --depth;
    return ok;
  }
};

// Moves fields[key] to out if it is of that kind.
static bool Take(std::map<std::string, Value>& fields, const char* key, Value::Kind kind, Value& out) {
  const auto it = fields.find(key);
  if (it == fields.end() || it->second.kind != kind) return false;
  out = std::move(it->second);
  fields.erase(it);
  return true;
}

static bool Whole(std::string_view t, uint64_t max, uint64_t& out) {
  const auto [end, ec] = std::from_chars(t.data(), t.data() + t.size(), out);
  return ec == std::errc() && end == t.data() + t.size() && out <= max;
}

// An optional string field: one of `names`, as its index; `fallback` when absent.
static bool Pick(std::map<std::string, Value>& fields, const char* key, std::initializer_list<std::string_view> names,
                 int fallback, int& out) {
  Value v;
  out = fallback;
  if (!fields.contains(key)) return true;
  if (!Take(fields, key, Value::kString, v)) return false;
  const auto it = std::find(names.begin(), names.end(), v.text);
  out = int(it - names.begin());
  return it != names.end();
}

// One cam.path keyframe, as serde reads sweep.rs's Keyframe: numbers t and fov, three-number pos and rot, an optional
// ease, nothing else.
static bool ReadKey(std::string_view text, sweep::Key& k) {
  std::map<std::string, Value> f;
  Parser p{text};
  Value t, fov, pos, rot;
  int ease = 0;
  if (!p.Object(f) || !Take(f, "t", Value::kNumber, t) || !ParseDouble(t.text, k.t) ||
      !Take(f, "fov", Value::kNumber, fov) || !ParseDouble(fov.text, k.fov) || !Take(f, "pos", Value::kArray, pos) ||
      !Take(f, "rot", Value::kArray, rot) || pos.items.size() != 3 || rot.items.size() != 3 ||
      !Pick(f, "ease", {"linear", "ease_in_out"}, 0, ease))
    return false;
  for (int c = 0; c < 3; ++c)
    if (!ParseDouble(pos.items[c], k.pos[c]) || !ParseDouble(rot.items[c], k.rot[c])) return false;
  k.ease = ease == 1;
  return f.empty();
}

// cam.path's fields, as protocol.rs's Verb::check checks them: sweep::check_playback, then Path::new, then canonical
// keyframes. Empty, or why not.
static std::string CamPath(std::map<std::string, Value>& fields, Command& cmd) {
  auto path = std::make_shared<sweep::Path>();
  int mode = 0, speed = 0, ease = 0;
  Value fps, keys;
  uint64_t rate = 0;
  if (!Pick(fields, "mode", {"realtime", "step"}, -1, mode) || mode < 0) return "mode is realtime or step";
  if (fields.contains("fps") && !Take(fields, "fps", Value::kNull, fps) &&
      !(Take(fields, "fps", Value::kNumber, fps) && Whole(fps.text, UINT32_MAX, rate) && rate))
    return "fps is a whole number from 1";
  if ((mode == 1) != (rate != 0) || rate > sweep::kMaxFps)
    return "frame step takes 1 to 240 frames per second (fps), real time none";
  if (!Pick(fields, "speed", {"keys", "constant"}, 0, speed)) return "speed is keys or constant";
  if (!Pick(fields, "ease", {"linear", "ease_in_out"}, 0, ease)) return "ease is linear or ease_in_out";
  if (!Take(fields, "keys", Value::kArray, keys)) return "keys is an array of keyframes";
  for (size_t i = 0; i < keys.items.size(); ++i)
    if (!ReadKey(keys.items[i], path->keys.emplace_back()))
      return "keyframe " + std::to_string(i) + ": not {t, pos, rot, fov, ease?} of numbers";
  path->constant = speed == 1;
  path->ease = ease == 1;
  path->fps = uint32_t(rate);
  const std::string why = sweep::Build(*path);
  if (!why.empty()) return why;
  cmd.args = mode == 1 ? " 1 " + std::to_string(path->frames) : " 0 0";
  cmd.path = std::move(path);
  return "";
}

// One request line (no newline). False: error says why, and cmd.id holds the id if it was readable (else 0).
static bool Decode(std::string_view line, Command& cmd, std::string& error) {
  if (line.size() > kMaxRequest) {
    error = "message of " + std::to_string(line.size()) + " bytes, the limit is " + std::to_string(kMaxRequest);
    return false;
  }
  std::map<std::string, Value> fields;
  Parser p{line};
  bool ok = ValidUtf8(line) && p.Object(fields);
  p.Ws();
  if (!ok || p.i != line.size()) {
    error = "malformed message: not one flat JSON object with unique keys";
    return false;
  }

  const auto take = [&](const char* key, Value::Kind kind, Value& out) { return Take(fields, key, kind, out); };
  const auto whole = [&](const char* key, uint64_t max, uint64_t& out) {
    Value v;
    return take(key, Value::kNumber, v) && Whole(v.text, max, out);
  };
  const auto vec3 = [&](const char* key) {
    Value v;
    if (!take(key, Value::kArray, v) || v.items.size() != 3) return false;
    for (const std::string& t : v.items) {
      float x;
      if (!ParseFloat(t, x)) return false;
      cmd.args += ' ' + Fixed(x);
    }
    return true;
  };
  const auto name = [&](const char* key) {
    Value v;
    if (!take(key, Value::kString, v) || v.text.empty()) return false;
    for (const unsigned char c : v.text)
      if (c < 0x20) return false;
    cmd.args = ' ' + v.text;
    return true;
  };

  uint64_t ttl = 0, n = 0, hour = 0;
  Value verb;
  if (!whole("id", UINT64_MAX, cmd.id)) {
    cmd.id = 0;
    error = "malformed message: no id (a whole number)";
    return false;
  }
  if (!whole("ttl_ms", UINT32_MAX, ttl) || !take("verb", Value::kString, verb)) {
    error = "malformed message: ttl_ms (a whole number below 2^32) and verb (a string) are required";
    return false;
  }
  cmd.ttl_ms = uint32_t(ttl);
  cmd.verb = verb.text;
  const std::string& v = cmd.verb;
  if (v == "load" || v == "weather") {
    ok = name(v == "load" ? "save" : "name");
  } else if (v == "notice") {
    ok = name("text") && cmd.args.size() <= kMaxNotice + 1;
  } else if (v == "loadlast" || v == "listsaves" || v == "cam.get" || v == "cam.stop" || v == "save" ||
             v == "world.get" || v == "photo.get") {
    ok = true;
  } else if (v == "photo.weather" || v == "photo.env") {
    ok = name(v == "photo.env" ? "path" : "name");
  } else if (v == "photo.time") {
    Value x;
    float m = 0;
    ok = take("minute", Value::kNumber, x) && ParseFloat(x.text, m) && m >= 0 && m < 1440;
    cmd.args = ' ' + Fixed(m);
  } else if (v == "photo.grade" || v == "photo.tick") {
    // Each value as "<given 0|1> <value>", in a fixed order, so the script sets only the given ones; at least one.
    const std::vector<const char*> keys = v == "photo.tick" ? std::vector<const char*>{"tick_s", "cloth_s"}
                                                            : std::vector<const char*>(std::begin(kGrade), std::end(kGrade));
    bool any = false;
    for (const char* key : keys) {
      Value x;
      float f = 0;
      const bool given = fields.contains(key);
      if (given)
        ok = ok && take(key, Value::kNumber, x) && ParseFloat(x.text, f) && (v != "photo.tick" || (f >= 0 && f <= kMaxTick));
      any = any || given;
      cmd.args += given ? " 1 " + Fixed(f) : std::string(" 0 0");
    }
    ok = ok && any;
  } else if (v == "items.add") {
    // "<count> <equip 0|1> <name>": the name last, as it may hold spaces.
    Value equip;
    ok = whole("count", kMaxItemCount, n) && n > 0 && take("equip", Value::kBool, equip) && name("name") &&
         cmd.args.size() <= kMaxItem + 1;
    cmd.args = ' ' + std::to_string(n) + (equip.flag ? " 1" : " 0") + cmd.args;
  } else if (v == "world.actors") {
    // "<max> <radius>".
    Value r;
    float radius = 0;
    ok = whole("max", kMaxActors, n) && n > 0 && take("radius", Value::kNumber, r) && ParseFloat(r.text, radius) &&
         radius > 0 && radius <= kMaxActorsRadius;
    cmd.args = ' ' + std::to_string(n) + ' ' + Fixed(radius);
  } else if (v == "photo.reset") {
    const char* parts[] = {"weather", "env", "grading"};
    int part = -1;
    ok = Pick(fields, "part", {parts[0], parts[1], parts[2]}, -1, part) && part >= 0;
    if (ok) cmd.args = std::string(" ") + parts[part];
  } else if (v == "photo.list") {
    int what = -1;
    ok = Pick(fields, "what", {"weathers", "envs"}, -1, what) && what >= 0;
    const std::string head = what == 1 ? " envs" : " weathers";
    if (ok && fields.contains("filter")) ok = name("filter") && cmd.args.size() <= kMaxFilter + 1;
    cmd.args = head + cmd.args;
  } else if (v == "cam.path") {
    if (const std::string why = CamPath(fields, cmd); !why.empty()) {
      error = "invalid cam.path: " + why;
      return false;
    }
  } else if (v == "cam.frame") {
    // ponytail: frames capped at the script's int range, as settime's day
    ok = whole("n", INT32_MAX, n);
    cmd.args = ' ' + std::to_string(n);
  } else if (v == "cam.set" || v == "teleport") {
    ok = vec3("pos") && vec3("rot");
    Value fov;
    float f = 0;
    if (ok && v == "cam.set" && fields.contains("fov") && !take("fov", Value::kNull, fov)) {  // null: no fov
      ok = take("fov", Value::kNumber, fov) && ParseFloat(fov.text, f) && f > 0;
      cmd.args += ' ' + Fixed(f);
    }
  } else if (v == "settime") {
    // ponytail: day capped at the script's int range, a stricter limit than protocol.rs's u32
    ok = whole("day", INT32_MAX, n) && whole("hour", 23, hour);
    cmd.args = ' ' + std::to_string(n) + ' ' + std::to_string(hour);
  } else if (v == "pause" || v == "photo.mode") {
    Value on;
    ok = take("on", Value::kBool, on);
    cmd.args = on.flag ? " 1" : " 0";
  } else {
    error = "unknown verb " + Quote(v);
    return false;
  }
  if (!ok) {
    error = "invalid " + v + ": a field is missing, of the wrong type or out of range (non-finite, empty, a control "
            "character, fov not above 0, hour past 23, a notice over 160 bytes, a photo.time minute outside 0 to 1440, a "
            "photo.grade or photo.tick without a value, a photo.tick time outside 0 to 10 s, a filter over 200 bytes, an "
            "items.add name over 200 bytes or count outside 1 to 1000, a world.actors radius outside 0 (excluded) to 100 or "
            "max outside 1 to 50)";
    return false;
  }
  if (!fields.empty()) {
    error = "unknown field " + Quote(fields.begin()->first) + " for " + v;
    return false;
  }
  return true;
}

static void HandleLine(std::string_view line) {
  if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
  Command cmd;
  std::string error;
  const bool ok = Decode(line, cmd, error);
  std::lock_guard lock(g_mutex);
  if (!ok) {
    Log("refused id %llu: %s", cmd.id, error.c_str());
    SendLocked(ReplyLine(cmd.id, false, "refused: " + error));
  } else if (g_inbox.size() >= kMaxQueued) {
    Log("refused id %llu: queue full", cmd.id);
    SendLocked(ReplyLine(cmd.id, false, "refused: " + std::to_string(kMaxQueued) +
                                             " commands already wait; the game doesn't poll at the main menu"));
  } else {
    cmd.arrived = GetTickCount64();
    g_inbox.push_back(std::move(cmd));
  }
}

// ---- the script side ----

static std::string ReadString(void* frame) {
  WNB_String s{};
  // WNB 2.0 (Remastered) hands UTF-8 bytes despite the wchar_t*, size counting the NUL. The engine buffer is never
  // freed: WNB has no call for it (ponytail: one small leak per reply; listsaves leaks its list once per call).
  if (WNB_ReadStringParameter(frame, &s) <= 1 || !s.data) return {};
  return {reinterpret_cast<const char*>(s.data), s.size - 1};
}

// import function QUEN_Next() : string;  "" or "<handle> <verb><args>"
static void Next(void*, void* frame, void* result) {
  std::string line;
  {
    std::lock_guard lock(g_mutex);
    if (++g_polls == 1) g_first_poll = GetTickCount64();
    if (g_current.handle) {
      Log("id %llu: no reply from the script", g_current.id);
      SendLocked(ReplyLine(g_current.id, false, "modQuen gave no reply (a script error?)"));
      g_current = {};
    }
    const ULONGLONG now = GetTickCount64();
    while (!g_inbox.empty() && line.empty()) {
      Command cmd = std::move(g_inbox.front());
      g_inbox.pop_front();
      if (now - cmd.arrived > cmd.ttl_ms) {
        Log("id %llu: expired", cmd.id);
        SendLocked(ReplyLine(cmd.id, false, "expired: waited " + std::to_string(now - cmd.arrived) + " ms, ttl " +
                                                std::to_string(cmd.ttl_ms) +
                                                " ms (the game doesn't poll at the main menu)"));
        continue;
      }
      cmd.handle = g_next_handle;
      g_next_handle = g_next_handle == INT32_MAX ? 1 : g_next_handle + 1;
      line = std::to_string(cmd.handle) + ' ' + cmd.verb + cmd.args;
      if (cmd.path) {  // the script loads it, or refuses it and unloads its own
        g_path = std::move(cmd.path);
        g_path_start = 0;
      }
      Log("id %llu: to the script as handle %d, %s", cmd.id, cmd.handle, cmd.verb.c_str());
      g_current = std::move(cmd);
    }
  }
  std::wstring wide(line.size(), L'\0');  // UTF-8 never takes more UTF-16 units than bytes
  wide.resize(line.empty() ? 0 : MultiByteToWideChar(CP_UTF8, 0, line.data(), int(line.size()), wide.data(), int(wide.size())));
  WNB_ReturnString(frame, result, wide.c_str(), uint32_t(wide.size()));
}

// The script's read-back as reply data: listsaves and photo.list "a|b|c" (no file name, weather name or env path holds a
// '|') -> ["a","b","c"]; cam.get, cam.set, cam.path and cam.frame "<camera> x y z pitch yaw roll fov" ->
// {"camera":..,"pos":[..],"rot":[..],"fov":..}; world.get "x y z heading day hour rain snow skyclear weather" ->
// {"player":{"pos":[..],"heading":..},"time":{"day":..,"hour":..},"rain":..,"snow":..,"sky_clear":..,"weather":..};
// items.add "before after equipped" (the item's count before and after, equipped 0 or 1) ->
// {"before":..,"after":..,"equipped":..};
// world.actors "<found>" then per actor "|x y z heading vx vy vz distance flags|<name>|<entity>" (modQuen's
// QUENRPC_Actors: flags 1 combat, 2 hostile, 4 guard, 8 at work, 16 action point, 32 scene, 64 velocity known, 128
// entity cut, from quen-w3 0.1.11) ->
// {"found":..,"actors":[{"name":..,"entity":..,"pos":[..],"heading":..,"velocity":[..] or null,"distance":..,
// "combat":..,"hostile":..,"guard":..,"at_work":..,"action_point":..,"scene":..,"entity_cut":..}]};
// the other photo.* verbs modQuen's QUENRPC_PhotoState (20 numbers, then '|' and three names) ->
// {"weather":..,"minute":..,"env":..,"grading":{..},"grading_at_open":{..},"rain":..,"snow":..,"sky_clear":..,
// "world_weather":..}.
static bool DataJson(const std::string& verb, std::string_view data, std::string& json) {
  if (verb == "listsaves" || verb == "photo.list") {
    json = "[";
    if (!data.empty())
      for (const std::string_view item : Split(data, '|')) json += Quote(item) + ',';
    if (json.back() == ',') json.pop_back();
    json += ']';
    return true;
  }
  if (verb == "world.actors") {
    const std::vector<std::string_view> part = Split(data, '|');
    uint64_t found = 0;
    if ((part.size() - 1) % 3 || !Whole(part[0], INT32_MAX, found) || (part.size() - 1) / 3 > found) return false;
    std::string out = "{\"found\":" + std::to_string(found) + ",\"actors\":[";  // json only once all of it parsed
    for (size_t a = 1; a < part.size(); a += 3) {
      const std::vector<std::string_view> t = Split(part[a], ' ');
      float f[8];
      uint64_t flags = 0;
      if (t.size() != 9 || !Whole(t[8], 255, flags)) return false;
      for (int k = 0; k < 8; ++k)
        if (!ParseFloat(t[k], f[k])) return false;
      const auto flag = [&](uint64_t bit) { return std::string(flags & bit ? "true" : "false"); };
      const std::string velocity =
          flags & 64 ? "[" + Number(f[4]) + ',' + Number(f[5]) + ',' + Number(f[6]) + ']' : std::string("null");
      out += std::string(a > 1 ? "," : "") + "{\"name\":" + Quote(part[a + 1]) + ",\"entity\":" + Quote(part[a + 2]) +
              ",\"pos\":[" + Number(f[0]) + ',' + Number(f[1]) + ',' + Number(f[2]) + "],\"heading\":" + Number(f[3]) +
              ",\"velocity\":" + velocity + ",\"distance\":" + Number(f[7]) + ",\"combat\":" + flag(1) +
              ",\"hostile\":" + flag(2) + ",\"guard\":" + flag(4) + ",\"at_work\":" + flag(8) +
              ",\"action_point\":" + flag(16) + ",\"scene\":" + flag(32) +
              ",\"entity_cut\":" + flag(128) + '}';
    }
    json = out + "]}";
    return true;
  }
  const std::vector<std::string_view> t = Split(data, ' ');
  float v[7];
  if (verb.starts_with("cam.") && t.size() == 8) {
    for (int k = 0; k < 7; ++k)
      if (!ParseFloat(t[k + 1], v[k])) return false;
    json = "{\"camera\":" + Quote(t[0]) + ",\"pos\":[" + Number(v[0]) + ',' + Number(v[1]) + ',' + Number(v[2]) +
           "],\"rot\":[" + Number(v[3]) + ',' + Number(v[4]) + ',' + Number(v[5]) + "],\"fov\":" + Number(v[6]) + '}';
    return true;
  }
  uint64_t before = 0, after = 0;
  if (verb == "items.add" && t.size() == 3) {
    if (!Whole(t[0], INT32_MAX, before) || !Whole(t[1], INT32_MAX, after) || (t[2] != "0" && t[2] != "1")) return false;
    json = "{\"before\":" + std::to_string(before) + ",\"after\":" + std::to_string(after) +
           ",\"equipped\":" + (t[2] == "1" ? "true" : "false") + '}';
    return true;
  }
  // rain, snow and the sky flag, "r s 0|1" as QUENRPC_Env writes them, as JSON members.
  const auto env = [](std::string_view rain, std::string_view snow, std::string_view sky, std::string& out) {
    float r = 0, s = 0;
    if (!ParseFloat(rain, r) || !ParseFloat(snow, s) || (sky != "0" && sky != "1")) return false;
    out = "\"rain\":" + Number(r) + ",\"snow\":" + Number(s) + ",\"sky_clear\":" + (sky == "1" ? "true" : "false");
    return true;
  };
  std::string weather;
  if (verb == "world.get" && t.size() == 10) {
    uint64_t day = 0, hour = 0;
    for (int k = 0; k < 4; ++k)
      if (!ParseFloat(t[k], v[k])) return false;
    if (!Whole(t[4], INT32_MAX, day) || !Whole(t[5], 23, hour) || !env(t[6], t[7], t[8], weather)) return false;
    json = "{\"player\":{\"pos\":[" + Number(v[0]) + ',' + Number(v[1]) + ',' + Number(v[2]) + "],\"heading\":" +
           Number(v[3]) + "},\"time\":{\"day\":" + std::to_string(day) + ",\"hour\":" + std::to_string(hour) + "}," +
           weather + ",\"weather\":" + Quote(t[9]) + '}';
    return true;
  }
  const std::vector<std::string_view> part = Split(data, '|');
  if (!verb.starts_with("photo.") || part.size() != 4) return false;
  const std::vector<std::string_view> n = Split(part[0], ' ');
  float f[17];
  if (n.size() != 20 || !env(n[17], n[18], n[19], weather)) return false;
  for (int k = 0; k < 17; ++k)
    if (!ParseFloat(n[k], f[k])) return false;
  const auto grading = [&](int at) {
    std::string out = "{";
    for (int k = 0; k < 8; ++k) out += (k ? ",\"" : "\"") + std::string(kGrade[k]) + "\":" + Number(f[at + k]);
    return out + '}';
  };
  json = "{\"weather\":" + Quote(part[1]) + ",\"minute\":" + Number(f[0]) + ",\"env\":" + Quote(part[2]) +
         ",\"grading\":" + grading(1) + ",\"grading_at_open\":" + grading(9) + ',' + weather +
         ",\"world_weather\":" + Quote(part[3]) + '}';
  return true;
}

// import function QUEN_Reply(handle : int, ok : bool, text : string, data : string);
static void Reply(void*, void* frame, void*) {
  const int handle = WNB_ReadIntParameter(frame);
  bool ok = WNB_ReadBoolParameter(frame);
  std::string text = ReadString(frame);
  const std::string data = ReadString(frame);
  WNB_ReturnVoid(frame);

  std::lock_guard lock(g_mutex);
  if (!handle || handle != g_current.handle) {
    Log("QUEN_Reply for handle %d, not the current command: ignored", handle);
    return;
  }
  std::string json;
  // no saves or no match: "data":[]
  const bool has_data = !data.empty() || (ok && (g_current.verb == "listsaves" || g_current.verb == "photo.list"));
  if (!ValidUtf8(text)) {
    ok = false;
    text = "modQuen's reply text is not valid UTF-8";
  } else if (has_data && (!ValidUtf8(data) || !DataJson(g_current.verb, data, json))) {
    ok = false;
    text = "modQuen's read-back did not parse: " + (ValidUtf8(data) ? data.substr(0, 200) : "(not UTF-8)");
  }
  Log("id %llu: reply ok=%d", g_current.id, ok);
  SendLocked(ReplyLine(g_current.id, ok, text, json));
  g_current = {};
}

// import function QUEN_CamAt(frame : int) : string;  the pose to set this tick on the path of the last cam.path handed to
// the script, as "x y z pitch yaw roll fov": frame `frame` of a frame-step path (its time frame / fps,
// computed, never summed), or with frame < 0 a real-time path at the time since its first QUEN_CamAt, holding the last
// pose after the end. Real time is the performance counter's: not GetEngineTimeAsSeconds, which moves in 31.25 ms steps
// (the probe boot). "" without a path, or for a frame that isn't one of the path's.
static void CamAt(void*, void* frame, void* result) {
  const int n = WNB_ReadIntParameter(frame);
  std::string pose;
  if (const sweep::Path* p = g_path.get()) {
    double t = -1;
    if (p->fps && n >= 0 && uint32_t(n) < p->frames) {
      t = std::min(double(n) / p->fps, p->Duration());
    } else if (!p->fps && n < 0) {
      static const double hz = [] {
        LARGE_INTEGER f;
        QueryPerformanceFrequency(&f);
        return double(f.QuadPart);
      }();
      LARGE_INTEGER now;
      QueryPerformanceCounter(&now);
      if (!g_path_start) g_path_start = now.QuadPart;
      t = double(now.QuadPart - g_path_start) / hz;
    }
    if (t >= 0) {
      const sweep::Pose s = p->Sample(t);
      for (const double x : {s.pos[0], s.pos[1], s.pos[2], s.rot[0], s.rot[1], s.rot[2], s.fov})
        pose += (pose.empty() ? "" : " ") + Fixed(x);
    }
  }
  const std::wstring wide(pose.begin(), pose.end());  // ASCII
  WNB_ReturnString(frame, result, wide.c_str(), uint32_t(wide.size()));
}

// import function QUEN_State(flags : int);  modQuen's game state, sent when it changes. With a token, one
// "<tick> push=N flags=F" line in W3QuenState.log: the W3 Game State Bridge's line form, which quen-core's bridge_log
// reads. The first one also logs how many polls came before it, and the first poll's tick: against the launch and the
// main menu's time, it says whether the game ticks at the main menu.
static void State(void*, void* frame, void*) {
  const int flags = WNB_ReadIntParameter(frame);
  WNB_ReturnVoid(frame);
  if (!g_token) return;
  std::lock_guard lock(g_mutex);
  static FILE* const log = OpenBesideExe(L"W3QuenState.log");
  static unsigned pushes = 0;
  if (++pushes == 1)
    Log("first QUEN_State (flags %d) after %llu QUEN_Next calls, the first at tick %llu", flags, g_polls, g_first_poll);
  if (!log) return;
  fprintf(log, "%llu push=%u flags=%d\n", GetTickCount64(), pushes, flags);
  fflush(log);
}

// ---- the pipe ----

// D:P(A;;GA;;;<the current user's SID>): nobody else, not even administrators or SYSTEM, opens the pipe.
static bool UserOnly(SECURITY_ATTRIBUTES& sa) {
  HANDLE token;
  if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) return false;
  alignas(TOKEN_USER) BYTE buf[sizeof(TOKEN_USER) + SECURITY_MAX_SID_SIZE];
  DWORD len;
  const bool got = GetTokenInformation(token, TokenUser, buf, sizeof buf, &len);
  CloseHandle(token);
  wchar_t* sid = nullptr;
  if (!got || !ConvertSidToStringSidW(reinterpret_cast<TOKEN_USER*>(buf)->User.Sid, &sid)) return false;
  const std::wstring sddl = L"D:P(A;;GA;;;" + std::wstring(sid) + L")";
  LocalFree(sid);
  return ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, &sa.lpSecurityDescriptor,
                                                              nullptr);
}

// One connected client until it leaves or breaks the protocol: overlapped reads, and writes when the outbox fills.
static void Talk(HANDLE pipe, OVERLAPPED& rd, OVERLAPPED& wr) {
  char buf[4096];
  std::string pending;
  bool reading = false;
  for (;;) {
    if (!reading) {
      if (!ReadFile(pipe, buf, sizeof buf, nullptr, &rd) && GetLastError() != ERROR_IO_PENDING) break;
      reading = true;
    }
    const HANDLE waits[] = {rd.hEvent, g_wake};
    const DWORD woke = WaitForMultipleObjects(2, waits, FALSE, INFINITE);
    if (woke == WAIT_OBJECT_0) {
      DWORD got = 0;
      reading = false;
      if (!GetOverlappedResult(pipe, &rd, &got, FALSE)) break;
      pending.append(buf, got);
      size_t nl;
      while ((nl = pending.find('\n')) != std::string::npos) {
        HandleLine(std::string_view(pending).substr(0, nl));
        pending.erase(0, nl + 1);
      }
      if (pending.size() > kMaxRequest + 1) {
        Log("a request line over %zu bytes: disconnecting", kMaxRequest);
        break;
      }
    } else if (woke == WAIT_OBJECT_0 + 1) {
      std::deque<std::string> lines;
      {
        std::lock_guard lock(g_mutex);
        lines.swap(g_outbox);
      }
      bool written = true;
      for (const std::string& line : lines) {
        DWORD put = 0;
        written = (WriteFile(pipe, line.data(), DWORD(line.size()), nullptr, &wr) || GetLastError() == ERROR_IO_PENDING)
                  && GetOverlappedResult(pipe, &wr, &put, TRUE) && put == line.size();
        if (!written) break;
      }
      if (!written) break;
    } else {
      break;
    }
  }
  if (reading) {
    DWORD got;
    CancelIoEx(pipe, &rd);
    GetOverlappedResult(pipe, &rd, &got, TRUE);  // buf must outlive the cancelled read
  }
}

static DWORD WINAPI Serve(void*) {
  if (!g_log) g_log = OpenBesideExe(L"W3Quen.log");  // a tracy launch opened it in DllMain
  Log("W3Quen %s: natives queued=%d (WNB API %u)", kVersion, g_queued, WNB_GetApiVersion());

  wchar_t token[80]{}, pid_text[16]{};
  const DWORD token_len = GetEnvironmentVariableW(L"W3_QUEN_TOKEN", token, 80);
  bool token_ok = token_len >= 16 && token_len <= 64;
  for (DWORD k = 0; token_ok && k < token_len; ++k)
    token_ok = (token[k] < 128 && iswalnum(token[k])) || token[k] == L'-' || token[k] == L'_';
  if (!token_ok) {
    Log("W3_QUEN_TOKEN is not 16-64 characters of A-Z a-z 0-9 - _: no pipe");
    return 0;
  }
  const DWORD pid_len = GetEnvironmentVariableW(L"W3_QUEN_PID", pid_text, 16);
  wchar_t* pid_end = nullptr;
  const unsigned long peer = pid_len && pid_len < 16 ? wcstoul(pid_text, &pid_end, 10) : 0;
  if (!peer || *pid_end) {
    Log("W3_QUEN_PID is not a process id: no pipe");
    return 0;
  }

  SECURITY_ATTRIBUTES sa{sizeof sa, nullptr, FALSE};
  if (!UserOnly(sa)) {
    Log("could not build the current-user ACL (error %lu): no pipe", GetLastError());
    return 0;
  }
  const std::wstring name = L"\\\\.\\pipe\\quen-w3-" + std::wstring(token, token_len);
  const HANDLE pipe = CreateNamedPipeW(name.c_str(), PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                                       PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 1,
                                       DWORD(kMaxReply), DWORD(kMaxRequest), 0, &sa);
  LocalFree(sa.lpSecurityDescriptor);
  if (pipe == INVALID_HANDLE_VALUE) {
    Log("CreateNamedPipe failed, error %lu (5: the name is taken): no pipe", GetLastError());
    return 0;
  }
  Log("pipe ready for pid %lu", peer);

  OVERLAPPED rd{}, wr{};
  rd.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  wr.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  for (;;) {
    DWORD unused;
    BOOL connected = ConnectNamedPipe(pipe, &rd);
    if (!connected && GetLastError() == ERROR_IO_PENDING) connected = GetOverlappedResult(pipe, &rd, &unused, TRUE);
    if (connected || GetLastError() == ERROR_PIPE_CONNECTED) {
      ULONG client = 0;
      if (GetNamedPipeClientProcessId(pipe, &client) && client == peer) {
        Log("client pid %lu connected", client);
        Talk(pipe, rd, wr);
        Log("client left");
      } else {
        Log("refused a client: pid %lu, expected %lu", client, peer);
      }
    } else {
      Log("ConnectNamedPipe failed, error %lu", GetLastError());
      Sleep(100);
    }
    DisconnectNamedPipe(pipe);
    std::lock_guard lock(g_mutex);  // the client is gone: its commands and replies go with it
    g_inbox.clear();
    g_outbox.clear();
    g_current = {};
  }
}

BOOL APIENTRY DllMain(HMODULE module, DWORD reason, LPVOID) {
  if (reason == DLL_PROCESS_ATTACH) {
    // A tracy launch keeps thread notifications: the static CRT's and Tracy's per-thread data use them (W3Trace).
    if (!trace::Triggered()) DisableThreadLibraryCalls(module);
    HMODULE pinned;  // the engine and the pipe thread keep pointers into this module
    GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                       reinterpret_cast<LPCWSTR>(&DllMain), &pinned);
    // Queue only (WNB registers them inside the engine's own native registration). False: an older WNB, or the names
    // already taken (a second copy of this ASI).
    g_queued = WNB_GetApiVersion() >= WNB_API_VERSION && WNB_RegisterNative("QUEN_Next", &Next)
               && WNB_RegisterNative("QUEN_Reply", &Reply) && WNB_RegisterNative("QUEN_State", &State)
               && WNB_RegisterNative("QUEN_CamAt", &CamAt);
    // Inert without a token: no thread, no files, no pipe. The thread checks the token and pid, not the loader lock.
    g_token = GetEnvironmentVariableW(L"W3_QUEN_TOKEN", nullptr, 0) != 0;
    // Only quen's tracy launch profile sets the trigger; without it nothing of trace.h runs.
    if (trace::Triggered()) {
      g_log = OpenBesideExe(L"W3Quen.log");
      trace::Init();
    }
    if (g_token) {
      g_wake = CreateEventW(nullptr, FALSE, FALSE, nullptr);
      if (const HANDLE thread = CreateThread(nullptr, 0, &Serve, nullptr, 0, nullptr)) CloseHandle(thread);
    }
  }
  return TRUE;
}
