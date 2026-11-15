#include <windows.h>
#include <cstring>
#include <cstdio>
int main(int argc, char** argv) {
    const bool loop = argc > 1 && !std::strcmp(argv[1], "loop");
    OutputDebugStringA("probe:target-start\n");
    if (loop) {
        char name[80];sprintf_s(name,"Local\\DbgAtlas3A-%lu",GetCurrentProcessId());
        auto h=CreateFileMappingA(INVALID_HANDLE_VALUE,nullptr,PAGE_READWRITE,0,4096,name);
        auto count=static_cast<volatile LONG*>(MapViewOfFile(h,FILE_MAP_ALL_ACCESS,0,0,4096));
        for (;;) {InterlockedIncrement(count);Sleep(10);}
    }
    if(argc>1 && !std::strcmp(argv[1],"record")) { for(int i=0;i<3000000;++i) {volatile auto n=GetTickCount64();(void)n;}return 37; }
    Sleep(600);
    DebugBreak();
    OutputDebugStringA("probe:after-break\n");
    Sleep(300);
    return 37;
}
