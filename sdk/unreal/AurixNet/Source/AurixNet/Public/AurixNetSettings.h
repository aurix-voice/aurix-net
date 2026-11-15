#pragma once
#include "CoreMinimal.h"
#include "Engine/DeveloperSettings.h"
#include "AurixNetSettings.generated.h"

/** Project Settings → Plugins → Aurix Net. Shipped with the game (DefaultGame.ini); never put keys here. */
UCLASS(config = Game, defaultconfig, meta = (DisplayName = "Aurix Net"))
class AURIXNET_API UAurixNetSettings : public UDeveloperSettings
{
	GENERATED_BODY()
public:
	UAurixNetSettings();
	virtual FName GetCategoryName() const override { return FName(TEXT("Plugins")); }

	/** Cell the game connects to, e.g. https://eu-central.net.aurix.run (hosted) or your own cell. */
	UPROPERTY(config, EditAnywhere, Category = "Connection") FString CellUrl;
	/** Always use the WebSocket tunnel instead of QUIC datagrams (restrictive networks, debugging). */
	UPROPERTY(config, EditAnywhere, Category = "Connection") bool bForceTunnel = false;
	/** Max players when a room is created without an explicit value (NetDriver ?listen, async nodes). */
	UPROPERTY(config, EditAnywhere, Category = "Rooms", meta = (ClampMin = 1, ClampMax = 65535)) int32 DefaultMaxPlayers = 16;
	/** When the master leaves a relay room, clients stay and follow the new master (seamless travel to aurix://<room>). */
	UPROPERTY(config, EditAnywhere, Category = "Rooms") bool bHostMigration = true;
	/** Interest grid cell size in Unreal units for AurixInterestComponent. */
	UPROPERTY(config, EditAnywhere, Category = "Interest", meta = (ClampMin = 100)) float InterestCellSize = 5000.f;

	static const UAurixNetSettings* Get() { return GetDefault<UAurixNetSettings>(); }
};

/** Editor-only (per user, never cooked): development SDK key used to mint grants for PIE. */
UCLASS(config = EditorPerProjectUserSettings, meta = (DisplayName = "Aurix Net (Editor)"))
class AURIXNET_API UAurixNetEditorSettings : public UDeveloperSettings
{
	GENERATED_BODY()
public:
	virtual FName GetCategoryName() const override { return FName(TEXT("Plugins")); }
	/** Development SDK key (Console → Networking → Keys & grants). Stays on this machine; PIE mints a grant per instance with it. */
	UPROPERTY(config, EditAnywhere, Category = "Development") FString DevApiKey;
	/** Player id prefix for PIE instances (pie-0, pie-1, ...). */
	UPROPERTY(config, EditAnywhere, Category = "Development") FString DevPlayerPrefix = TEXT("pie");
};
