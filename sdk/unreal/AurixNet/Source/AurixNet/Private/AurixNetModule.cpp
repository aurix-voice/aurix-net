#include "AurixNetLog.h"
#include "AurixNetDriver.h"
#include "Modules/ModuleManager.h"
#include "Interfaces/IPluginManager.h"
#include "HAL/PlatformProcess.h"
#include "Misc/Paths.h"

DEFINE_LOG_CATEGORY(LogAurixNet);

class FAurixNetModule : public IModuleInterface
{
public:
	virtual void StartupModule() override
	{
#if PLATFORM_WINDOWS
		const TSharedPtr<IPlugin> Plugin = IPluginManager::Get().FindPlugin(TEXT("AurixNet"));
		if (Plugin.IsValid())
		{
			const FString Dll = FPaths::Combine(Plugin->GetBaseDir(), TEXT("Source/ThirdParty/AurixNetLibrary/lib/Win64/aurix_net.dll"));
			LibHandle = FPlatformProcess::GetDllHandle(*Dll);
			if (!LibHandle) { UE_LOG(LogAurixNet, Warning, TEXT("aurix_net.dll not found at %s; relying on the packaged binary"), *Dll); }
		}
#endif
		UE_LOG(LogAurixNet, Log, TEXT("Aurix Net plugin loaded"));
	}
	virtual void ShutdownModule() override
	{
		if (LibHandle) { FPlatformProcess::FreeDllHandle(LibHandle); LibHandle = nullptr; }
	}
private:
	void* LibHandle = nullptr;
};

IMPLEMENT_MODULE(FAurixNetModule, AurixNet)
