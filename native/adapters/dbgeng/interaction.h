// Private implementation. No COM or C++ types cross the exported C ABI.

struct InteractionState final {
    std::mutex control_mutex;
    IDebugControl* control = nullptr; // borrowed; invalidated before owner releases COM
    uint64_t epoch = 0;
    uint32_t activity = 0;
    int32_t native_hresult = S_OK;
    bool submitted = false;
    bool exit_requested = false;
    std::mutex events_mutex;
    std::deque<DA_DbgEngEvent> events;
    uint64_t sequence = 0;
    uint64_t dropped = 0;
    std::atomic<uint64_t> event_epoch{0};
    std::atomic<uint64_t> allocation_gaps{0};
    HANDLE output = INVALID_HANDLE_VALUE;
    uint64_t output_bytes = 0;
    DWORD output_error = ERROR_SUCCESS;

    ~InteractionState() { if (output != INVALID_HANDLE_VALUE) CloseHandle(output); }

    void record(uint32_t kind, uint32_t code, uint64_t argument = 0) noexcept {
        // Callbacks never take the control mutex: SetInterrupt may invoke a callback.
        try {
            std::lock_guard lock(events_mutex);
            if (events.size() == 1024) { events.pop_front(); ++dropped; }
            events.push_back({++sequence, event_epoch.load(), argument, kind, code});
        } catch (...) { ++allocation_gaps; }
    }
};

class SessionEventCallbacks final : public DebugBaseEventCallbacks {
public:
    explicit SessionEventCallbacks(std::shared_ptr<InteractionState> state) : state_(std::move(state)) {}
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs_); }
    ULONG STDMETHODCALLTYPE Release() override { return InterlockedDecrement(&refs_); }
    HRESULT STDMETHODCALLTYPE GetInterestMask(PULONG mask) override {
        if (!mask) return E_POINTER;
        *mask = DEBUG_EVENT_BREAKPOINT | DEBUG_EVENT_EXCEPTION | DEBUG_EVENT_EXIT_PROCESS | DEBUG_EVENT_CHANGE_ENGINE_STATE;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Breakpoint(PDEBUG_BREAKPOINT bp) override {
        ULONG id = DEBUG_ANY_ID; ULONG64 address = 0;
        if (bp) { bp->GetId(&id); bp->GetOffset(&address); }
        state_->record(1, id, address);
        return DEBUG_STATUS_NO_CHANGE;
    }
    HRESULT STDMETHODCALLTYPE Exception(PEXCEPTION_RECORD64 e, ULONG first) override {
        state_->record(2, e ? e->ExceptionCode : 0, first);
        return DEBUG_STATUS_NO_CHANGE; // Preserve debugger exception filters.
    }
    HRESULT STDMETHODCALLTYPE ExitProcess(ULONG code) override {
        state_->record(3, code);
        return DEBUG_STATUS_NO_CHANGE;
    }
    HRESULT STDMETHODCALLTYPE ChangeEngineState(ULONG flags, ULONG64 argument) override {
        if (flags & DEBUG_CES_EXECUTION_STATUS) state_->record(4, flags, argument);
        return DEBUG_STATUS_NO_CHANGE;
    }
private:
    volatile LONG refs_ = 1;
    std::shared_ptr<InteractionState> state_;
};

class SessionOutputCallbacks final : public IDebugOutputCallbacks {
public:
    explicit SessionOutputCallbacks(std::shared_ptr<InteractionState> state) : state_(std::move(state)) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (id != __uuidof(IUnknown) && id != __uuidof(IDebugOutputCallbacks)) return E_NOINTERFACE;
        *out = this; AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs_); }
    ULONG STDMETHODCALLTYPE Release() override { return InterlockedDecrement(&refs_); }
    HRESULT STDMETHODCALLTYPE Output(ULONG, PCSTR text) override {
        if (!text) return S_OK;
        try {
            std::lock_guard lock(state_->events_mutex);
            if (state_->output_error || state_->output == INVALID_HANDLE_VALUE) return E_FAIL;
            size_t remaining = std::strlen(text);
            while (remaining) {
                DWORD written = 0;
                const DWORD chunk = static_cast<DWORD>(std::min<size_t>(remaining, MAXDWORD));
                if (!WriteFile(state_->output, text, chunk, &written, nullptr) || written != chunk) {
                    state_->output_error = GetLastError();
                    if (!state_->output_error) state_->output_error = ERROR_WRITE_FAULT;
                    return E_FAIL;
                }
                state_->output_bytes += written; remaining -= written; text += written;
            }
            return S_OK;
        } catch (...) { return E_FAIL; }
    }
private:
    volatile LONG refs_ = 1;
    std::shared_ptr<InteractionState> state_;
};

class SessionInputCallbacks final : public IDebugInputCallbacks {
public:
    explicit SessionInputCallbacks(std::shared_ptr<InteractionState> state) : state_(std::move(state)) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (id != __uuidof(IUnknown) && id != __uuidof(IDebugInputCallbacks)) return E_NOINTERFACE;
        *out = this; AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs_); }
    ULONG STDMETHODCALLTYPE Release() override { return InterlockedDecrement(&refs_); }
    HRESULT STDMETHODCALLTYPE StartInput(ULONG size) override { state_->record(5, 1, size); return S_OK; }
    HRESULT STDMETHODCALLTYPE EndInput() override { state_->record(5, 0); return S_OK; }
private:
    volatile LONG refs_ = 1;
    std::shared_ptr<InteractionState> state_;
};

struct InteractionEventsOwner final : ViewOwner { std::vector<DA_DbgEngEvent> events; };
struct InteractionHandle final { std::shared_ptr<InteractionState> state; };
