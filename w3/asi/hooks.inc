// The witcher3.exe functions W3Quen.asi hooks in a tracy launch (trace.h; nothing is hooked otherwise), ported from the
// W3Trace ASI's list without its three GPU-marker hooks. RVAs and bytes are Steam build 26238013's (PE stamp 0x6ADFBD7A;
// quen-w3 0.1.18 and earlier: 5.01's, 0x6AC555A3). W3_HOOK(name, RVA, its 16 entry bytes, signature, kind)
//   signature I: 8 integer/pointer slots; F1: slot 1 is a float (XMM1), forwarded as a double so all 64 bits pass
//   kind: plain (a zone), custom (a wrapper written by hand in trace.h), probe (a zone, and <name>_probe in trace.h
//   before and after the call), count (no zone: a register-transparent thunk counts calls, plotted per render frame)
// Each line cites the KB (witcher3-mods re\, the tables of build 26238013): its names.tsv row, and the hooks.tsv row of
// any other mod that changes the same site, which a tracy launch parks (crates/quen/src/profile.rs reads hooks.tsv for
// that). quen reads this file too.
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
W3_CALL(RemoteConsole_RunCommand, 0x24DBA50, "\x48\x8b\xc4\x55\x41\x56\x48\x8b\xec\x48\x83\xec\x78\x48\x8b\x0d")  // names.tsv 0x24DBA50 RemoteConsole run command; gates.tsv quen 0x24DD370 -> 0x24DBA50
W3_HOOK(CBaseEngine_Tick, 0x2251EB0, "\x40\x55\x53\x56\x57\x41\x56\x41\x57\x48\x8b\xec\x48\x83\xec\x58", F1, custom)  // names.tsv 0x2251EB0 CBaseEngine_Tick
W3_HOOK(CGame_Tick, 0x227B8D0, "\x40\x55\x57\x48\x8d\xac\x24\x98\x08\xff\xff\xb8\x68\xf8\x00\x00", F1, plain)  // names.tsv 0x227B8D0 CGame_Tick
W3_HOOK(RenderThread_ProcessCommands, 0x1C69650, "\x48\x89\x5c\x24\x18\x55\x56\x57\x41\x56\x41\x57\x48\x83\xec\x20", I, plain)  // names.tsv 0x1C69650 RenderThread_ProcessCommands
W3_HOOK(CRenderFence_Signal, 0x1CAB7F0, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x20\x48\x8b\xd9\x48\x83\xc1", I, plain)  // names.tsv 0x1CAB7F0 CRenderFence_Signal
W3_HOOK(Renderer_RenderScene, 0x1C19DC0, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x41\x54\x41\x55\x41\x56", I, plain)  // names.tsv 0x1C19DC0 Renderer_RenderScene
W3_HOOK(Render_FrameTaskGraph, 0x1D321A0, "\x48\x8b\xc4\x4c\x89\x48\x20\x4c\x89\x40\x18\x48\x89\x50\x10\x55", I, plain)  // names.tsv 0x1D321A0 Render_FrameTaskGraph
W3_HOOK(Render_EndOfFrameSubmit, 0x1CFA750, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x48\x89\x7c\x24\x18\x55", I, plain)  // names.tsv 0x1CFA750 Render_EndOfFrameSubmit
W3_HOOK(RenderCmd_EndFrame_Exec, 0x1C263B0, "\x48\x89\x5c\x24\x18\x48\x89\x7c\x24\x20\x41\x56\x48\x83\xec\x20", I, custom)  // names.tsv 0x1C263B0 RenderCmd_EndFrame_Exec
W3_HOOK(RenderNormalTaskBatch_Run, 0x1D5FA30, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x57", I, plain)  // names.tsv 0x1D5FA30 RenderNormalTaskBatch_Run
W3_HOOK(RenderNormalEpilogueTaskBatch_Run, 0x1D5EBA0, "\x48\x8b\xc4\x48\x89\x58\x20\x55\x56\x57\x41\x54\x41\x55\x41\x56", I, plain)  // names.tsv 0x1D5EBA0 RenderNormalEpilogueTaskBatch_Run
W3_HOOK(RenderFinal2DTaskBatch_Run, 0x1D608E0, "\x48\x8b\xc4\x53\x57\x48\x81\xec\xe8\x00\x00\x00\x48\x89\x70\xe8", I, plain)  // names.tsv 0x1D608E0 RenderFinal2DTaskBatch_Run
W3_HOOK(RenderEarlyParticleTaskBatch_Run, 0x1D5F700, "\x48\x8b\xc4\x48\x89\x58\x08\x48\x89\x70\x10\x48\x89\x78\x18\x55", I, plain)  // names.tsv 0x1D5F700 RenderEarlyParticleTaskBatch_Run
W3_HOOK(SceneCullingTask_RunStages, 0x1BF92E0, "\x48\x89\x5c\x24\x10\x57\x48\x83\xec\x30\x8b\x41\x40\x8b\xfa\x48", I, plain)  // names.tsv 0x1BF92E0 SceneCullingTask_RunStages
W3_HOOK(SceneCulling_OcclusionStage, 0x1BFB890, "\x40\x55\x53\x57\x41\x55\x48\x8d\xac\x24\xf8\x67\xfe\xff\xb8\x08", I, plain)  // names.tsv 0x1BFB890 SceneCulling_OcclusionStage
W3_HOOK(MeshBatcher_RenderMeshes, 0x1C7FDD0, "\x4c\x89\x44\x24\x18\x48\x89\x54\x24\x10\x55\x57\x41\x54\x41\x56", I, plain)  // names.tsv 0x1C7FDD0 MeshBatcher_RenderMeshes
W3_HOOK(ShadowCulling_Parallel, 0x1CF15E0, "\x4c\x89\x4c\x24\x20\x4c\x89\x44\x24\x18\x48\x89\x54\x24\x10\x48", I, plain)  // names.tsv 0x1CF15E0 ShadowCulling_Parallel
W3_HOOK(CRenderFence_Wait, 0x1CAB840, "\x40\x53\x48\x83\xec\x60\x48\x8b\xd9\xba\x64\x00\x00\x00\x48\x8b", I, plain)  // names.tsv 0x1CAB840 CRenderFence_Wait
W3_HOOK(RenderRing_Alloc, 0x2328E60, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, custom)  // names.tsv 0x2328E60 RenderRing_Alloc
W3_HOOK(JobCounter_Wait, 0x1D53270, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x30\x48\x8d\x44\x24\x20\x48", I, custom)  // names.tsv 0x1D53270 JobCounter_Wait
W3_HOOK(RenderDispatcher_JobPop, 0x1D53380, "\x48\x89\x5c\x24\x10\x57\x48\x83\xec\x30\x48\x8b\xfa\x48\xc7\x44", I, custom)  // names.tsv 0x1D53380 RenderDispatcher_JobPop
W3_HOOK(RTAS_WaitCollectAndPrebuild, 0x1F0EB40, "\x48\x89\x5c\x24\x18\x48\x89\x74\x24\x20\x55\x57\x41\x54\x48\x8b", I, plain)  // names.tsv 0x1F0EB40 RTAS_WaitCollectAndPrebuild
W3_HOOK(RenderRaytracingDataTaskBatch_Run, 0x1D5E780, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, plain)  // names.tsv 0x1D5E780 RenderRaytracingDataTaskBatch_Run
W3_HOOK(RenderRaytracingDispatchTaskBatch_Run, 0x1D60C80, "\x40\x53\x48\x83\xec\x20\x48\x8b\xd9\x48\x8b\x49\x28\x48\x83\xc1", I, plain)  // names.tsv 0x1D60C80 RenderRaytracingDispatchTaskBatch_Run
W3_HOOK(RenderRaytracingDataEpilogueTaskBatch_Run, 0x1D60580, "\x40\x55\x48\x83\xec\x40\x48\x89\x74\x24\x60\x48\x89\x7c\x24\x38", I, plain)  // names.tsv 0x1D60580 RenderRaytracingDataEpilogueTaskBatch_Run
W3_HOOK(RT_UpdateRayTracingData, 0x1C22050, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x53\x56\x57\x41\x54", I, plain)  // names.tsv 0x1C22050 RT_UpdateRayTracingData; hooks.tsv 0x1C22050 hook W3EngineTweaks 1.12.4 (RT split consumer)
W3_HOOK(DxRT_CollectTlasAndShaderTable, 0x1EEC0B0, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x1EEC0B0 DxRT_CollectTlasAndShaderTable
W3_HOOK(RT_CollectTlasTask_Run, 0x1F0BEF0, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x57", I, plain)  // names.tsv 0x1F0BEF0 RT_CollectTlasTask_Run
W3_HOOK(RT_TlasEmplaceBatch_Run, 0x05108A0, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x5108A0 RT_TlasEmplaceBatch_Run
W3_HOOK(RTAS_RecordBuilds, 0x1F0E440, "\x48\x89\x5c\x24\x20\x48\x89\x4c\x24\x08\x55\x56\x57\x41\x54\x41", I, plain)  // names.tsv 0x1F0E440 RTAS_RecordBuilds
W3_HOOK(PT_MainPass, 0x1C1DD50, "\x48\x89\x54\x24\x10\x55\x53\x56\x57\x41\x54\x41\x55\x41\x56\x41", I, plain)  // names.tsv 0x1C1DD50 PT_MainPass
W3_HOOK(HairWorks_BuildOrUpdateBLASes, 0x2819EB0, "\x40\x53\x55\x41\x54\x41\x57\x48\x83\xec\x38\x48\x8b\xe9\x4c\x8b", I, plain)  // names.tsv 0x2819EB0 HairWorks_BuildOrUpdateBLASes
W3_HOOK(RTCull_RunStages, 0x1BF8AF0, "\x40\x53\x48\x83\xec\x30\x8b\x41\x40\x48\x8b\xd9\x83\xf8\x03\x0f", I, plain)  // names.tsv 0x1BF8AF0 RTCull_RunStages
W3_HOOK(RTCull_ProximityWalk, 0x1CF4340, "\x48\x8b\xc4\x48\x89\x58\x08\x48\x89\x70\x10\x48\x89\x78\x18\x55", I, probe)  // names.tsv 0x1CF4340 RTCull_ProximityWalk
W3_HOOK(RTQueue_PushReady, 0x1CD6790, "\x41\x54\x41\x55\x41\x56\x41\x57\x48\x8b\x05\xa9\x31\xd8\x03\x45", I, probe)  // names.tsv 0x1CD6790 RTQueue_PushReady
W3_HOOK(RTCull_Join, 0x1BF1330, "\x40\x53\x48\x83\xec\x20\x65\x48\x8b\x04\x25\x58\x00\x00\x00\x48", I, plain)  // names.tsv 0x1BF1330 RTCull_Join
W3_HOOK(RT_MeshListSubmit, 0x1BF4AE0, "\x48\x89\x5c\x24\x08\x48\x89\x6c\x24\x10\x48\x89\x74\x24\x18\x48", I, plain)  // names.tsv 0x1BF4AE0 RT_MeshListSubmit; hooks.tsv 0x1BF4AE0 hook W3EngineTweaks 1.12.4 (RT split per-batch mesh update)
W3_HOOK(RT_MeshBatcherMaterials, 0x1C83B00, "\x4c\x8b\xdc\x4d\x89\x43\x18\x49\x89\x4b\x08\x55\x57\x41\x55\x49", I, plain)  // names.tsv 0x1C83B00 RT_MeshBatcherMaterials
W3_HOOK(RT_TerrainMaterialSync, 0x1D26FD0, "\x40\x55\x53\x57\x41\x55\x41\x57\x48\x8d\x6c\x24\xc9\x48\x81\xec", I, plain)  // names.tsv 0x1D26FD0 RT_TerrainMaterialSync
W3_HOOK(SpeedTree_RenderProxy, 0x1CE6B40, "\x48\x89\x54\x24\x10\x55\x56\x57\x41\x55\x41\x56\x48\x8d\xac\x24", I, plain)  // names.tsv 0x1CE6B40 SpeedTree_RenderProxy
W3_HOOK(ScenePass_Render, 0x1C27AA0, "\x48\x89\x4c\x24\x08\x55\x53\x48\x8d\xac\x24\xf8\xef\xff\xff\xb8", I, plain)  // names.tsv 0x1C27AA0 ScenePass_Render
W3_HOOK(Lighting_Render, 0x1C37910, "\x48\x89\x4c\x24\x08\x55\x56\x57\x41\x54\x41\x56\x41\x57\x48\x8d", I, plain)  // names.tsv 0x1C37910 Lighting_Render
W3_HOOK(Lighting_ShadowMask, 0x1C6C7C0, "\x48\x8b\xc4\x4c\x89\x48\x20\x48\x89\x50\x10\x48\x89\x48\x08\x55", I, plain)  // names.tsv 0x1C6C7C0 Lighting_ShadowMask
W3_HOOK(Lighting_AO, 0x1C146D0, "\x44\x89\x4c\x24\x20\x53\x56\x57\x48\x81\xec\xf0\x00\x00\x00\x48", I, plain)  // names.tsv 0x1C146D0 Lighting_AO
W3_HOOK(Lighting_RTLights, 0x1C1D1F0, "\x48\x89\x54\x24\x10\x55\x56\x41\x56\x48\x8d\xac\x24\xc0\xf2\xff", I, plain)  // names.tsv 0x1C1D1F0 Lighting_RTLights
W3_HOOK(Lighting_TiledDeferred, 0x1C6D2C0, "\x48\x8b\xc4\x44\x89\x40\x18\x55\x53\x56\x57\x41\x54\x41\x55\x41", I, plain)  // names.tsv 0x1C6D2C0 Lighting_TiledDeferred
W3_HOOK(Lighting_SSS, 0x1C6F2A0, "\x48\x8b\xc4\x48\x89\x58\x10\x48\x89\x70\x18\x44\x89\x48\x20\x55", I, plain)  // names.tsv 0x1C6F2A0 Lighting_SSS
W3_HOOK(Lighting_HBIL, 0x1C2CF80, "\x48\x89\x5c\x24\x10\x48\x89\x74\x24\x18\x55\x57\x41\x54\x41\x55", I, plain)  // names.tsv 0x1C2CF80 Lighting_HBIL
W3_HOOK(Denoiser_PackSignals, 0x1C310A0, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1C310A0 Denoiser_PackSignals
W3_HOOK(PT_TracePass, 0x1C1ECB0, "\x48\x89\x5c\x24\x18\x48\x89\x74\x24\x20\x55\x57\x41\x54\x41\x56", I, plain)  // names.tsv 0x1C1ECB0 PT_TracePass
W3_HOOK(PT_GIComposite, 0x1C20B40, "\x48\x8b\xc4\x44\x88\x48\x20\x4c\x89\x40\x18\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1C20B40 PT_GIComposite
W3_HOOK(SSR_Render, 0x1C35590, "\x48\x89\x4c\x24\x08\x53\x57\x41\x56\x41\x57\x48\x81\xec\xb8\x04", I, plain)  // names.tsv 0x1C35590 SSR_Render
W3_HOOK(PostChain_Render, 0x1C0DFE0, "\x4c\x89\x4c\x24\x20\x55\x53\x56\x57\x41\x54\x41\x56\x41\x57\x48", I, plain)  // names.tsv 0x1C0DFE0 PostChain_Render
W3_HOOK(Post_Exposure, 0x1C13C70, "\x48\x8b\xc4\x44\x89\x48\x20\x55\x53\x41\x57\x48\x8d\xa8\xa8\xfd", I, plain)  // names.tsv 0x1C13C70 Post_Exposure
W3_HOOK(Post_Upscaler, 0x1C08D80, "\x48\x8b\xc4\x55\x53\x56\x57\x41\x54\x41\x56\x41\x57\x48\x8d\xa8", I, plain)  // names.tsv 0x1C08D80 Post_Upscaler
W3_HOOK(Post_Bloom, 0x1C0B870, "\x40\x55\x41\x56\x48\x8d\xac\x24\x88\xf1\xff\xff\x48\x81\xec\x78", I, plain)  // names.tsv 0x1C0B870 Post_Bloom
W3_HOOK(Post_Tonemap, 0x034DA80, "\x48\x8b\xc4\x44\x88\x48\x20\x48\x89\x50\x10\x48\x89\x48\x08\x55", I, plain)  // names.tsv 0x34DA80 Post_Tonemap
W3_HOOK(Post_FinalGrade, 0x1C0B640, "\x48\x89\x5c\x24\x10\x48\x89\x6c\x24\x18\x48\x89\x74\x24\x20\x57", I, plain)  // names.tsv 0x1C0B640 Post_FinalGrade
W3_HOOK(GBuffer_Fill, 0x1D1F6B0, "\x48\x8b\xc4\x48\x89\x50\x10\x48\x89\x48\x08\x55\x53\x56\x57\x41", I, plain)  // names.tsv 0x1D1F6B0 GBuffer_Fill
W3_HOOK(LocalLightShadows, 0x1C34E50, "\x40\x53\x56\x48\x83\xec\x38\x48\x8b\x42\x10\x48\x8b\xda\x48\x8b", I, plain)  // names.tsv 0x1C34E50 LocalLightShadows; hooks.tsv 0x1C34E50 hook W3EngineTweaks 1.12.4 (local-shadow offload)
W3_HOOK(DepthPrepass, 0x1C349F0, "\x48\x8b\xc4\x41\x55\x41\x57\x48\x83\xec\x68\x4c\x8b\x7a\x10\x48", I, plain)  // names.tsv 0x1C349F0 DepthPrepass
W3_HOOK(RT_ShadowsPass, 0x1C1CE20, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x48\x89\x7c\x24\x18\x4c", I, plain)  // names.tsv 0x1C1CE20 RT_ShadowsPass
W3_HOOK(PT_HairPass, 0x1C1EFC0, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x57\x41\x54\x41\x55\x41", I, plain)  // names.tsv 0x1C1EFC0 PT_HairPass
W3_HOOK(PT_LightList, 0x1C1F980, "\x44\x88\x44\x24\x18\x48\x89\x4c\x24\x08\x53\x55\x56\x57\x41\x54", I, probe)  // names.tsv 0x1C1F980 PT_LightList
W3_HOOK(PT_LightSort, 0x0353330, "\x48\x89\x5c\x24\x08\x48\x89\x74\x24\x10\x57\x48\x83\xec\x60\x8b", I, probe)  // names.tsv 0x353330 PT_LightSort; hooks.tsv 0x353330 hook W3LightCache 1.1 (PT light sort)
W3_HOOK(PT_LightSortLoop, 0x1C19910, "\x40\x57\x41\x56\x41\x57\x48\x83\xec\x60\x4d\x63\xd8\x45\x8b\xf9", I, custom)  // names.tsv 0x1C19910 PT_LightSortLoop
W3_HOOK(LightBlock_Upload, 0x1C71F40, "\x40\x53\x48\x83\xec\x30\x48\x8b\x81\x98\x06\x00\x00\x48\x8b\xd9", I, probe)  // names.tsv 0x1C71F40 LightBlock_Upload
W3_HOOK(Submit_Phase, 0x03899A0, "\x48\x89\x4c\x24\x08\x53\x56\x41\x56\x48\x83\xec\x60\x83\x79\x40", I, probe)  // names.tsv 0x3899A0 Submit_Phase
W3_HOOK(Submit_Fixups, 0x0509AC0, "\x40\x53\x55\x41\x57\x48\x83\xec\x40\x48\x8b\x1a\x4c\x8b\xf9\x48", I, probe)  // names.tsv 0x509AC0 Submit_Fixups
W3_HOOK(Submit_Execute, 0x0509F20, "\x48\x8b\xc4\x48\x89\x50\x10\x55\x53\x56\x57\x41\x56\x48\x8d\x68", I, plain)  // names.tsv 0x509F20 Submit_Execute
W3_HOOK(GpuCtx_Close, 0x1EF3FB0, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x20\x48\x8d\xb9\xc0\x04\x21", I, probe)  // names.tsv 0x1EF3FB0 GpuCtx_Close
W3_HOOK(RTSBT_Build, 0x1F22420, "\x48\x89\x4c\x24\x08\x55\x53\x56\x57\x41\x54\x41\x55\x48\x8d\x6c", I, probe)  // names.tsv 0x1F22420 RTSBT_Build
W3_HOOK(RTMat_SlotPush, 0x1F21200, "\x40\x57\x48\x83\xec\x50\x8b\xc2\x41\x8b\xf9\x4c\x8b\xc9\x48\x8d", I, count)  // names.tsv 0x1F21200 RTMat_SlotPush
W3_HOOK(RTSkin_ChunkDispatch, 0x1D11C30, "\x44\x88\x4c\x24\x20\x44\x89\x44\x24\x18\x55\x57\x41\x55\x41\x57", I, count)  // names.tsv 0x1D11C30 RTSkin_ChunkDispatch
W3_HOOK(RTCollect_MeshProxy, 0x1D2A300, "\x48\x89\x5c\x24\x08\x57\x48\x83\xec\x30\xf6\x81\x20\x01\x00\x00", I, count)  // names.tsv 0x1D2A300 RTCollect_MeshProxy
W3_HOOK(Barrier_Add, 0x1EDD3B0, "\x40\x53\x41\x56\x48\x83\xec\x38\x83\x3a\x00\x48\x8b\xda\x4c\x8b", I, count)  // names.tsv 0x1EDD3B0 Barrier_Add
#ifdef W3_HOOK_EMPTY
#undef W3_HOOK
#undef W3_HOOK_EMPTY
#endif
#ifdef W3_CALL_EMPTY
#undef W3_CALL
#undef W3_CALL_EMPTY
#endif
