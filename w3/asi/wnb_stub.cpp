// Stand-in WitcherNativeBridge.asi for build.bat test only (out\test, never installed): it records WNB_RegisterNative
// and runs a registered native on a fake frame, as the script VM would, so test.cpp can call QUEN_Next, QUEN_Reply,
// QUEN_State and QUEN_CamAt.
// Strings arrive as WNB 2.0 hands them on Remastered: UTF-8 bytes behind the wchar_t*, size counting the NUL.
#define WITCHER_NATIVE_BRIDGE_EXPORTS
#include <windows.h>
#include <deque>
#include <map>
#include <string>
#include "WitcherNativeBridgeApi.h"

struct Frame {
  std::deque<int> ints;
  std::deque<bool> bools;
  std::deque<std::string> strings;  // read in order; kept alive until the call ends
  std::deque<std::string> read;
  std::wstring returned;
  int returns = 0;  // a native must finish its frame exactly once
};

static std::map<std::string, WNB_NativeImplementation> g_natives;

extern "C" {
uint32_t WNB_GetApiVersion() { return WNB_API_VERSION; }
bool WNB_RegisterNative(const char* name, WNB_NativeImplementation implementation) {
  return g_natives.emplace(name, implementation).second;
}
int WNB_ReadIntParameter(void* frame) {
  auto& f = *static_cast<Frame*>(frame);
  const int v = f.ints.front();
  f.ints.pop_front();
  return v;
}
bool WNB_ReadBoolParameter(void* frame) {
  auto& f = *static_cast<Frame*>(frame);
  const bool v = f.bools.front();
  f.bools.pop_front();
  return v;
}
float WNB_ReadFloatParameter(void*) { return 0; }
WNB_Name WNB_ReadNameParameter(void*) { return 0; }
int WNB_ReadStringParameter(void* frame, WNB_String* text) {
  auto& f = *static_cast<Frame*>(frame);
  f.read.push_back(std::move(f.strings.front()));
  f.strings.pop_front();
  text->data = reinterpret_cast<wchar_t*>(f.read.back().data());
  text->size = uint32_t(f.read.back().size() + 1);
  return int(text->size);
}
void WNB_ReturnVoid(void* frame) { ++static_cast<Frame*>(frame)->returns; }
void WNB_ReturnInt(void* frame, void*, int) { ++static_cast<Frame*>(frame)->returns; }
void WNB_ReturnBool(void* frame, void*, bool) { ++static_cast<Frame*>(frame)->returns; }
void WNB_ReturnFloat(void* frame, void*, float) { ++static_cast<Frame*>(frame)->returns; }
bool WNB_ReturnString(void* frame, void*, const wchar_t* value, uint32_t length) {
  auto& f = *static_cast<Frame*>(frame);
  f.returned.assign(value, length);
  ++f.returns;
  return true;
}
void WNB_ReturnName(void* frame, void*, WNB_Name) { ++static_cast<Frame*>(frame)->returns; }
void WNB_ReturnNameFromString(void* frame, void*, const wchar_t*) { ++static_cast<Frame*>(frame)->returns; }

// For test.cpp (plain C types: each module has its own static CRT).
__declspec(dllexport) bool Stub_Registered(const char* name) { return g_natives.contains(name); }

// QUEN_Next into out (cap wide chars with the NUL); its length, or -1 if not registered or the frame ended badly.
__declspec(dllexport) int Stub_Next(wchar_t* out, int cap) {
  const auto it = g_natives.find("QUEN_Next");
  if (it == g_natives.end()) return -1;
  Frame f;
  it->second(nullptr, &f, nullptr);
  if (f.returns != 1 || int(f.returned.size()) >= cap) return -1;
  wcscpy_s(out, cap, f.returned.c_str());
  return int(f.returned.size());
}

// QUEN_CamAt(frame) into out, as Stub_Next.
__declspec(dllexport) int Stub_CamAt(int frame, wchar_t* out, int cap) {
  const auto it = g_natives.find("QUEN_CamAt");
  if (it == g_natives.end()) return -1;
  Frame f;
  f.ints = {frame};
  it->second(nullptr, &f, nullptr);
  if (f.returns != 1 || !f.ints.empty() || int(f.returned.size()) >= cap) return -1;
  wcscpy_s(out, cap, f.returned.c_str());
  return int(f.returned.size());
}

// QUEN_Reply(handle, ok, text, data): 0, or -1 as above.
__declspec(dllexport) int Stub_Reply(int handle, bool ok, const char* text, const char* data) {
  const auto it = g_natives.find("QUEN_Reply");
  if (it == g_natives.end()) return -1;
  Frame f;
  f.ints = {handle};
  f.bools = {ok};
  f.strings = {text, data};
  it->second(nullptr, &f, nullptr);
  return f.returns == 1 && f.ints.empty() && f.bools.empty() && f.strings.empty() ? 0 : -1;
}

// QUEN_State(flags): 0, or -1 as above.
__declspec(dllexport) int Stub_State(int flags) {
  const auto it = g_natives.find("QUEN_State");
  if (it == g_natives.end()) return -1;
  Frame f;
  f.ints = {flags};
  it->second(nullptr, &f, nullptr);
  return f.returns == 1 && f.ints.empty() ? 0 : -1;
}
}
