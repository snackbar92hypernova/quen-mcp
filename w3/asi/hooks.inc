// The witcher3.exe 5.01 functions W3Quen.asi hooks in a tracy launch (trace.h; nothing is hooked otherwise), ported
// from the W3Trace ASI's list without its three GPU-marker hooks. W3_HOOK(name, RVA, its 16 entry bytes, signature, kind)
//   signature I: 8 integer/pointer slots; F1: slot 1 is a float (XMM1), forwarded as a double so all 64 bits pass
//   kind: plain (a zone), custom (a wrapper written by hand in trace.h), probe (a zone, and <name>_probe in trace.h
//   before and after the call), count (no zone: a register-transparent thunk counts calls, plotted per render frame)
// Each line cites the KB (witcher3-mods re\): its names.tsv row, and the hooks.tsv row of any other mod that changes the
// same site, which a tracy launch parks (crates/quen/src/profile.rs reads hooks.tsv for that). quen reads this file too.
// W3_CALL(name, RVA, its 16 entry bytes): a function W3Quen.asi calls and never changes (from quen-w3 0.1.13), its bytes
// checked against the running exe before each call. An includer defines the macro it wants; the other one is empty.
#ifndef W3_HOOK
#define W3_HOOK(NAME, RVA, BYTES, SIG, KIND)
#define W3_HOOK_EMPTY
#endif
#ifndef W3_CALL
#define W3_CALL(NAME, RVA, BYTES)
#define W3_CALL_EMPTY
#endif
W3_CALL(RemoteConsole_RunCommand, 0x24DD370, "\x48\x8b\xc4\x55\x41\x56\x48\x8b\xec\x48\x83\xec\x78\x48\x8b\x0d")  // incoming\console-exec-20261020.tsv 0x24DD370 RemoteConsole run command; hooks.tsv 0x24DD370 call W3Quen exec verb
W3_HOOK(CBaseEngine_Tick, 0x2253730, "\x40\x55\x53\x56\x57\x41\x56\x41\x57\x48\x8b\xec\x48\x83\xec\x58", F1, custom)  // names.tsv 0x2253730 CBaseEngine_Tick
W3_HOOK(CGame_Tick, 0x227D1F0, "\x40\x55\x57\x48\x8d\xac\x24\x98\x08\xff\xff\xb8\x68\xf8\x00\x00", F1, plain)  // names.tsv 0x227D1F0 CGame_Tick
W3_HOOK(RenderThread_ProcessCommands, 0x1C69AE0, "\x48\x89\x5c\x24\x18\x55\x56\x57\x41\x56\x41\x57\x48\x83\xec\x20", I, plain)  // names.tsv 0x1C69AE0 RenderThread_ProcessCommands
W3_HOOK(CRenderFence_Signal, 0x1CABD90, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x20\x48\x8b\xd9\x48\x83\xc1", I, plain)  // names.tsv 0x1CABD90 CRenderFence_Signal
W3_HOOK(Renderer_RenderScene, 0x1C1A210, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x41\x54\x41\x55\x41\x56", I, plain)  // names.tsv 0x1C1A210 Renderer_RenderScene
W3_HOOK(Render_FrameTaskGraph, 0x1D326A0, "\x48\x8b\xc4\x4c\x89\x48\x20\x4c\x89\x40\x18\x48\x89\x50\x10\x55", I, plain)  // names.tsv 0x1D326A0 Render_FrameTaskGraph
W3_HOOK(Render_EndOfFrameSubmit, 0x1CFAC70, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x48\x89\x7c\x24\x18\x55", I, plain)  // names.tsv 0x1CFAC70 Render_EndOfFrameSubmit
W3_HOOK(RenderCmd_EndFrame_Exec, 0x1C267F0, "\x48\x89\x5c\x24\x18\x48\x89\x7c\x24\x20\x41\x56\x48\x83\xec\x20", I, custom)  // names.tsv 0x1C267F0 RenderCmd_EndFrame_Exec
W3_HOOK(RenderNormalTaskBatch_Run, 0x1D5FE50, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x57", I, plain)  // names.tsv 0x1D5FE50 RenderNormalTaskBatch_Run
W3_HOOK(RenderNormalEpilogueTaskBatch_Run, 0x1D5EFC0, "\x48\x8b\xc4\x48\x89\x58\x20\x55\x56\x57\x41\x54\x41\x55\x41\x56", I, plain)  // names.tsv 0x1D5EFC0 RenderNormalEpilogueTaskBatch_Run
W3_HOOK(RenderFinal2DTaskBatch_Run, 0x1D60D00, "\x48\x8b\xc4\x53\x57\x48\x81\xec\xe8\x00\x00\x00\x48\x89\x70\xe8", I, plain)  // names.tsv 0x1D60D00 RenderFinal2DTaskBatch_Run
W3_HOOK(RenderEarlyParticleTaskBatch_Run, 0x1D5FB20, "\x48\x8b\xc4\x48\x89\x58\x08\x48\x89\x70\x10\x48\x89\x78\x18\x55", I, plain)  // names.tsv 0x1D5FB20 RenderEarlyParticleTaskBatch_Run
W3_HOOK(SceneCullingTask_RunStages, 0x1BF9770, "\x48\x89\x5c\x24\x10\x57\x48\x83\xec\x30\x8b\x41\x40\x8b\xfa\x48", I, plain)  // names.tsv 0x1BF9770 SceneCullingTask_RunStages
W3_HOOK(SceneCulling_OcclusionStage, 0x1BFBD20, "\x40\x55\x53\x57\x41\x55\x48\x8d\xac\x24\xf8\x67\xfe\xff\xb8\x08", I, plain)  // names.tsv 0x1BFBD20 SceneCulling_OcclusionStage
W3_HOOK(MeshBatcher_RenderMeshes, 0x1C80250, "\x4c\x89\x44\x24\x18\x48\x89\x54\x24\x10\x55\x57\x41\x54\x41\x56", I, plain)  // names.tsv 0x1C80250 MeshBatcher_RenderMeshes
W3_HOOK(ShadowCulling_Parallel, 0x1CF1AD0, "\x4c\x89\x4c\x24\x20\x4c\x89\x44\x24\x18\x48\x89\x54\x24\x10\x48", I, plain)  // names.tsv 0x1CF1AD0 ShadowCulling_Parallel
W3_HOOK(CRenderFence_Wait, 0x1CABDE0, "\x40\x53\x48\x83\xec\x60\x48\x8b\xd9\xba\x64\x00\x00\x00\x48\x8b", I, plain)  // names.tsv 0x1CABDE0 CRenderFence_Wait
W3_HOOK(RenderRing_Alloc, 0x232A780, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, custom)  // names.tsv 0x232A780 RenderRing_Alloc
W3_HOOK(JobCounter_Wait, 0x1D53740, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x30\x48\x8d\x44\x24\x20\x48", I, custom)  // names.tsv 0x1D53740 JobCounter_Wait
W3_HOOK(RenderDispatcher_JobPop, 0x1D53850, "\x48\x89\x5c\x24\x10\x57\x48\x83\xec\x30\x48\x8b\xfa\x48\xc7\x44", I, custom)  // names.tsv 0x1D53850 RenderDispatcher_JobPop
W3_HOOK(RTAS_WaitCollectAndPrebuild, 0x1F0F250, "\x48\x89\x5c\x24\x18\x48\x89\x74\x24\x20\x55\x57\x41\x54\x48\x8b", I, plain)  // names.tsv 0x1F0F250 RTAS_WaitCollectAndPrebuild
W3_HOOK(RenderRaytracingDataTaskBatch_Run, 0x1D5EBA0, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, plain)  // names.tsv 0x1D5EBA0 RenderRaytracingDataTaskBatch_Run
W3_HOOK(RenderRaytracingDispatchTaskBatch_Run, 0x1D610A0, "\x40\x53\x48\x83\xec\x20\x48\x8b\xd9\x48\x8b\x49\x28\x48\x83\xc1", I, plain)  // names.tsv 0x1D610A0 RenderRaytracingDispatchTaskBatch_Run
W3_HOOK(RenderRaytracingDataEpilogueTaskBatch_Run, 0x1D609A0, "\x40\x55\x48\x83\xec\x40\x48\x89\x74\x24\x60\x48\x89\x7c\x24\x38", I, plain)  // names.tsv 0x1D609A0 RenderRaytracingDataEpilogueTaskBatch_Run
W3_HOOK(RT_UpdateRayTracingData, 0x1C224A0, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x53\x56\x57\x41\x54", I, plain)  // names.tsv 0x1C224A0 RT_UpdateRayTracingData; hooks.tsv 0x1C224A0 hook W3EngineTweaks 1.12.4 (RT split consumer)
W3_HOOK(DxRT_CollectTlasAndShaderTable, 0x1EEC7A0, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x1EEC7A0 DxRT_CollectTlasAndShaderTable
W3_HOOK(RT_CollectTlasTask_Run, 0x1F0C600, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x57", I, plain)  // names.tsv 0x1F0C600 RT_CollectTlasTask_Run
W3_HOOK(RT_TlasEmplaceBatch_Run, 0x050F840, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x50F840 RT_TlasEmplaceBatch_Run
W3_HOOK(RTAS_RecordBuilds, 0x1F0EB50, "\x48\x89\x5c\x24\x20\x48\x89\x4c\x24\x08\x55\x56\x57\x41\x54\x41", I, plain)  // names.tsv 0x1F0EB50 RTAS_RecordBuilds
W3_HOOK(PT_MainPass, 0x1C1E1A0, "\x48\x89\x54\x24\x10\x55\x53\x56\x57\x41\x54\x41\x55\x41\x56\x41", I, plain)  // names.tsv 0x1C1E1A0 PT_MainPass
W3_HOOK(HairWorks_BuildOrUpdateBLASes, 0x283BE80, "\x40\x53\x55\x41\x54\x41\x57\x48\x83\xec\x38\x48\x8b\xe9\x4c\x8b", I, plain)  // names.tsv 0x283BE80 HairWorks_BuildOrUpdateBLASes
W3_HOOK(RTCull_RunStages, 0x1BF8F80, "\x40\x53\x48\x83\xec\x30\x8b\x41\x40\x48\x8b\xd9\x83\xf8\x03\x0f", I, plain)  // names.tsv 0x1BF8F80 RTCull_RunStages
W3_HOOK(RTCull_ProximityWalk, 0x1CF4830, "\x48\x8b\xc4\x48\x89\x58\x08\x48\x89\x70\x10\x48\x89\x78\x18\x55", I, probe)  // names.tsv 0x1CF4830 RTCull_ProximityWalk
W3_HOOK(RTQueue_PushReady, 0x1CD6CA0, "\x41\x54\x41\x55\x41\x56\x41\x57\x48\x8b\x05\x99\x4c\xd8\x03\x45", I, probe)  // names.tsv 0x1CD6CA0 RTQueue_PushReady
W3_HOOK(RTCull_Join, 0x1BF17A0, "\x40\x53\x48\x83\xec\x20\x65\x48\x8b\x04\x25\x58\x00\x00\x00\x48", I, plain)  // names.tsv 0x1BF17A0 RTCull_Join
W3_HOOK(RT_MeshListSubmit, 0x1BF4F60, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, plain)  // names.tsv 0x1BF4F60 RT_MeshListSubmit; hooks.tsv 0x1BF4F60 hook W3EngineTweaks 1.12.4 (RT split per-batch mesh update)
W3_HOOK(RT_MeshBatcherMaterials, 0x1C83F70, "\x4c\x8b\xdc\x4d\x89\x43\x18\x49\x89\x4b\x08\x55\x57\x41\x55\x49", I, plain)  // names.tsv 0x1C83F70 RT_MeshBatcherMaterials
W3_HOOK(RT_TerrainMaterialSync, 0x1D274E0, "\x40\x55\x53\x57\x41\x55\x41\x57\x48\x8d\x6c\x24\xc9\x48\x81\xec", I, plain)  // names.tsv 0x1D274E0 RT_TerrainMaterialSync
W3_HOOK(SpeedTree_RenderProxy, 0x1CE7030, "\x48\x89\x54\x24\x10\x55\x56\x57\x41\x55\x41\x56\x48\x8d\xac\x24", I, plain)  // names.tsv 0x1CE7030 SpeedTree_RenderProxy
W3_HOOK(ScenePass_Render, 0x1C27ED0, "\x48\x89\x4c\x24\x08\x55\x53\x48\x8d\xac\x24\xf8\xef\xff\xff\xb8", I, plain)  // names.tsv 0x1C27ED0 ScenePass_Render
W3_HOOK(Lighting_Render, 0x1C37DB0, "\x48\x89\x4c\x24\x08\x55\x56\x57\x41\x54\x41\x56\x41\x57\x48\x8d", I, plain)  // names.tsv 0x1C37DB0 Lighting_Render
W3_HOOK(Lighting_ShadowMask, 0x1C6CC20, "\x48\x8b\xc4\x4c\x89\x48\x20\x48\x89\x50\x10\x48\x89\x48\x08\x55", I, plain)  // names.tsv 0x1C6CC20 Lighting_ShadowMask
W3_HOOK(Lighting_AO, 0x1C14B20, "\x44\x89\x4c\x24\x20\x53\x56\x57\x48\x81\xec\xf0\x00\x00\x00\x48", I, plain)  // names.tsv 0x1C14B20 Lighting_AO
W3_HOOK(Lighting_RTLights, 0x1C1D640, "\x48\x89\x54\x24\x10\x55\x56\x41\x56\x48\x8d\xac\x24\xc0\xf2\xff", I, plain)  // names.tsv 0x1C1D640 Lighting_RTLights
W3_HOOK(Lighting_TiledDeferred, 0x1C6D720, "\x48\x8b\xc4\x44\x89\x40\x18\x55\x53\x56\x57\x41\x54\x41\x55\x41", I, plain)  // names.tsv 0x1C6D720 Lighting_TiledDeferred
W3_HOOK(Lighting_SSS, 0x1C6F710, "\x48\x8b\xc4\x48\x89\x58\x10\x48\x89\x70\x18\x44\x89\x48\x20\x55", I, plain)  // names.tsv 0x1C6F710 Lighting_SSS
W3_HOOK(Lighting_HBIL, 0x1C2D3B0, "\x48\x89\x5c\x24\x10\x48\x89\x74\x24\x18\x55\x57\x41\x54\x41\x55", I, plain)  // names.tsv 0x1C2D3B0 Lighting_HBIL
W3_HOOK(Denoiser_PackSignals, 0x1C31550, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1C31550 Denoiser_PackSignals
W3_HOOK(PT_TracePass, 0x1C1F100, "\x48\x89\x5c\x24\x18\x48\x89\x74\x24\x20\x55\x57\x41\x54\x41\x56", I, plain)  // names.tsv 0x1C1F100 PT_TracePass
W3_HOOK(PT_GIComposite, 0x1C20F90, "\x48\x8b\xc4\x44\x88\x48\x20\x4c\x89\x40\x18\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1C20F90 PT_GIComposite
W3_HOOK(SSR_Render, 0x1C35A30, "\x48\x89\x4c\x24\x08\x53\x57\x41\x56\x41\x57\x48\x81\xec\xb8\x04", I, plain)  // names.tsv 0x1C35A30 SSR_Render
W3_HOOK(PostChain_Render, 0x1C0E420, "\x4c\x89\x4c\x24\x20\x55\x53\x56\x57\x41\x54\x41\x56\x41\x57\x48", I, plain)  // names.tsv 0x1C0E420 PostChain_Render
W3_HOOK(Post_Exposure, 0x1C140C0, "\x48\x8b\xc4\x44\x89\x48\x20\x55\x53\x41\x57\x48\x8d\xa8\xa8\xfd", I, plain)  // names.tsv 0x1C140C0 Post_Exposure
W3_HOOK(Post_Upscaler, 0x1C091C0, "\x48\x8b\xc4\x55\x53\x56\x57\x41\x54\x41\x56\x41\x57\x48\x8d\xa8", I, plain)  // names.tsv 0x1C091C0 Post_Upscaler
W3_HOOK(Post_Bloom, 0x1C0BCB0, "\x40\x55\x41\x56\x48\x8d\xac\x24\x88\xf1\xff\xff\x48\x81\xec\x78", I, plain)  // names.tsv 0x1C0BCB0 Post_Bloom
W3_HOOK(Post_Tonemap, 0x034C880, "\x48\x8b\xc4\x44\x88\x48\x20\x48\x89\x50\x10\x48\x89\x48\x08\x55", I, plain)  // names.tsv 0x34C880 Post_Tonemap
W3_HOOK(Post_FinalGrade, 0x1C0BA80, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x1C0BA80 Post_FinalGrade
W3_HOOK(GBuffer_Fill, 0x1D1FC00, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1D1FC00 GBuffer_Fill
W3_HOOK(LocalLightShadows, 0x1C352F0, "\x40\x53\x56\x48\x83\xec\x38\x48\x8b\x42\x10\x48\x8b\xda\x48\x8b", I, plain)  // names.tsv 0x1C352F0 LocalLightShadows; hooks.tsv 0x1C352F0 hook W3EngineTweaks 1.12.4 (local-shadow offload)
W3_HOOK(DepthPrepass, 0x1C34E90, "\x48\x8b\xc4\x41\x55\x41\x57\x48\x83\xec\x68\x4c\x8b\x7a\x10\x48", I, plain)  // names.tsv 0x1C34E90 DepthPrepass
W3_HOOK(RT_ShadowsPass, 0x1C1D270, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x48\x89\x7c\x24\x18\x4c", I, plain)  // names.tsv 0x1C1D270 RT_ShadowsPass
W3_HOOK(PT_HairPass, 0x1C1F410, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x57\x41\x54\x41\x55\x41", I, plain)  // names.tsv 0x1C1F410 PT_HairPass
W3_HOOK(PT_LightList, 0x1C1FDD0, "\x44\x88\x44\x24\x18\x48\x89\x4c\x24\x08\x53\x55\x56\x57\x41\x54", I, probe)  // names.tsv 0x1C1FDD0 PT_LightList
W3_HOOK(PT_LightSort, 0x0352130, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x57\x48\x83\xec\x60\x8b", I, probe)  // names.tsv 0x352130 PT_LightSort; hooks.tsv 0x352130 hook W3LightCache 1.1 (PT light sort)
W3_HOOK(PT_LightSortLoop, 0x1C19D60, "\x40\x57\x41\x56\x41\x57\x48\x83\xec\x60\x4d\x63\xd8\x45\x8b\xf9", I, custom)  // names.tsv 0x1C19D60 PT_LightSortLoop
W3_HOOK(LightBlock_Upload, 0x1C723B0, "\x40\x53\x48\x83\xec\x30\x48\x8b\x81\x98\x06\x00\x00\x48\x8b\xd9", I, probe)  // names.tsv 0x1C723B0 LightBlock_Upload
W3_HOOK(Submit_Phase, 0x0388810, "\x48\x89\x4c\x24\x08\x53\x56\x41\x56\x48\x83\xec\x60\x83\x79\x40", I, probe)  // names.tsv 0x388810 Submit_Phase
W3_HOOK(Submit_Fixups, 0x0508A60, "\x40\x53\x55\x41\x57\x48\x83\xec\x40\x48\x8b\x1a\x4c\x8b\xf9\x48", I, probe)  // names.tsv 0x508A60 Submit_Fixups
W3_HOOK(Submit_Execute, 0x0508EC0, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x57\x41\x56\x48\x8d\x68", I, plain)  // names.tsv 0x508EC0 Submit_Execute
W3_HOOK(GpuCtx_Close, 0x1EF4680, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x20\x48\x8d\xb9\xc0\x04\x21", I, probe)  // names.tsv 0x1EF4680 GpuCtx_Close
W3_HOOK(RTSBT_Build, 0x1F22B70, "\x48\x89\x4c\x24\x08\x55\x53\x56\x57\x41\x54\x41\x55\x48\x8d\x6c", I, probe)  // names.tsv 0x1F22B70 RTSBT_Build
W3_HOOK(RTMat_SlotPush, 0x1F21950, "\x40\x57\x48\x83\xec\x50\x8b\xc2\x41\x8b\xf9\x4c\x8b\xc9\x48\x8d", I, count)  // names.tsv 0x1F21950 RTMat_SlotPush
W3_HOOK(RTSkin_ChunkDispatch, 0x1D12190, "\x44\x88\x4c\x24\x20\x44\x89\x44\x24\x18\x55\x57\x41\x55\x41\x57", I, count)  // names.tsv 0x1D12190 RTSkin_ChunkDispatch
W3_HOOK(RTCollect_MeshProxy, 0x1D2A810, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x30\xf6\x81\x20\x01\x00\x00", I, count)  // names.tsv 0x1D2A810 RTCollect_MeshProxy
W3_HOOK(Barrier_Add, 0x1EDDAA0, "\x40\x53\x41\x56\x48\x83\xec\x38\x83\x3a\x00\x48\x8b\xda\x4c\x8b", I, count)  // names.tsv 0x1EDDAA0 Barrier_Add
#ifdef W3_HOOK_EMPTY
#undef W3_HOOK
#undef W3_HOOK_EMPTY
#endif
#ifdef W3_CALL_EMPTY
#undef W3_CALL
#undef W3_CALL_EMPTY
#endif
