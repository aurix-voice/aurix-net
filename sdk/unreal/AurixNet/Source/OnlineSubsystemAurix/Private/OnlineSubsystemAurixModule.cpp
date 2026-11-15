#include "OnlineSubsystemAurix.h"
#include "OnlineSubsystemModule.h"
#include "Modules/ModuleManager.h"

DEFINE_LOG_CATEGORY(LogOnlineAurix);

class FOnlineFactoryAurix : public IOnlineFactory
{
public:
	virtual IOnlineSubsystemPtr CreateSubsystem(FName InstanceName) override
	{
		FOnlineSubsystemAurixPtr Sub = MakeShared<FOnlineSubsystemAurix, ESPMode::ThreadSafe>(InstanceName);
		if (Sub->IsEnabled() && Sub->Init()) return Sub;
		UE_LOG(LogOnlineAurix, Warning, TEXT("Aurix OSS disabled or failed to init"));
		Sub->Shutdown();
		return nullptr;
	}
};

class FOnlineSubsystemAurixModule : public IModuleInterface
{
public:
	virtual void StartupModule() override
	{
		Factory = MakeUnique<FOnlineFactoryAurix>();
		FOnlineSubsystemModule& OSS = FModuleManager::GetModuleChecked<FOnlineSubsystemModule>("OnlineSubsystem");
		OSS.RegisterPlatformService(AURIX_SUBSYSTEM, Factory.Get());
	}
	virtual void ShutdownModule() override
	{
		if (FOnlineSubsystemModule* OSS = FModuleManager::GetModulePtr<FOnlineSubsystemModule>("OnlineSubsystem")) OSS->UnregisterPlatformService(AURIX_SUBSYSTEM);
		Factory.Reset();
	}
	virtual bool SupportsDynamicReloading() override { return false; }
	virtual bool SupportsAutomaticShutdown() override { return false; }
private:
	TUniquePtr<FOnlineFactoryAurix> Factory;
};

IMPLEMENT_MODULE(FOnlineSubsystemAurixModule, OnlineSubsystemAurix);
