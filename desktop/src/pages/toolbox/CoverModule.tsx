import { CoverStudio } from '../CoverStudio';
import type { ToolboxModuleProps } from './shared';

/**
 * 做封面模块：直接复用 CoverStudio 的完整能力
 * （模板画廊、模板编辑、尺寸适配、批量生成与历史）。
 */
export function CoverModule({ isActive, onExecutionStateChange }: ToolboxModuleProps) {
    return (
        <CoverStudio
            isActive={isActive}
            onExecutionStateChange={onExecutionStateChange}
        />
    );
}
