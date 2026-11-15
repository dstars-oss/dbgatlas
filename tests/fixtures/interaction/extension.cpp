#include <windows.h>
#include <dbgeng.h>
extern "C" __declspec(dllexport) HRESULT CALLBACK DebugExtensionInitialize(PULONG version, PULONG flags) {
    *version = DEBUG_EXTENSION_VERSION(1, 0); *flags = 0; return S_OK;
}
extern "C" __declspec(dllexport) HRESULT CALLBACK cooperative(PDEBUG_CLIENT4 client, PCSTR) {
    IDebugControl* control = nullptr;
    if (FAILED(client->QueryInterface(__uuidof(IDebugControl), reinterpret_cast<void**>(&control)))) return E_FAIL;
    const auto start = GetTickCount64();
    while (GetTickCount64() - start < 5000) {
        if (control->GetInterrupt() == S_OK) { control->Output(DEBUG_OUTPUT_NORMAL, "cooperative:interrupted\n"); control->Release(); return E_ABORT; }
        Sleep(10);
    }
    control->Output(DEBUG_OUTPUT_NORMAL, "cooperative:finished\n"); control->Release(); return S_OK;
}
extern "C" __declspec(dllexport) HRESULT CALLBACK uncooperative(PDEBUG_CLIENT4 client, PCSTR) {
    Sleep(1500);
    IDebugControl* control = nullptr;
    if (SUCCEEDED(client->QueryInterface(__uuidof(IDebugControl), reinterpret_cast<void**>(&control)))) {
        control->Output(DEBUG_OUTPUT_NORMAL, "uncooperative:finished\n"); control->Release();
    }
    return S_OK;
}
